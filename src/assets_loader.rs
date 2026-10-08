use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use bevy::{
    prelude::*,
    render::{
        render_asset::RenderAssetUsages,
        render_resource::{
            Extent3d, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
        },
        texture::Image,
    },
    tasks::{AsyncComputeTaskPool, Task, futures_lite::future},
    window::PrimaryWindow,
};
use serde::{Deserialize, Serialize};

const VPK_SIGNATURE: u32 = 0x55aa_1234;
const VPK_INLINE_ARCHIVE: u16 = 0x7fff;
const MAX_VPK_TREE_SIZE: usize = 128 * 1024 * 1024;
const MAX_VPK_ENTRIES: usize = 2_000_000;
pub(super) const BSP_HEADER_SIZE: usize = 1036;

pub(super) struct SourceAssetLoaderPlugin {
    bootstrap: Bootstrap,
}

#[derive(States, Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub(super) enum AppState {
    #[default]
    Setup,
    Indexing,
    InGame,
}

impl SourceAssetLoaderPlugin {
    pub(super) fn new() -> Self {
        Self {
            bootstrap: Bootstrap::load(),
        }
    }
}

impl Plugin for SourceAssetLoaderPlugin {
    fn build(&self, app: &mut App) {
        app.insert_state(self.bootstrap.state)
            .insert_resource(self.bootstrap.config.clone())
            .insert_resource(SetupUiState {
                path: self
                    .bootstrap
                    .config
                    .depot_root
                    .as_ref()
                    .map_or_else(String::new, |path| path.display().to_string()),
                status: self.bootstrap.status.clone(),
                path_focused: true,
            })
            .init_resource::<VpkIndexing>()
            .init_resource::<LoaderScreenEntities>()
            .init_resource::<SourceAssetManager>()
            .add_systems(OnEnter(AppState::Setup), spawn_setup_screen)
            .add_systems(OnEnter(AppState::Indexing), begin_vpk_indexing)
            .add_systems(OnEnter(AppState::Indexing), spawn_indexing_screen)
            .add_systems(OnEnter(AppState::InGame), set_game_cursor)
            .add_systems(OnExit(AppState::Setup), despawn_loader_screen)
            .add_systems(OnExit(AppState::Indexing), despawn_loader_screen)
            .add_systems(
                Update,
                (handle_setup_input, update_setup_screen)
                    .chain()
                    .run_if(in_state(AppState::Setup)),
            )
            .add_systems(
                Update,
                (poll_vpk_indexing, update_indexing_screen)
                    .chain()
                    .run_if(in_state(AppState::Indexing)),
            );
    }
}

#[derive(Resource, Clone, Default)]
pub(super) struct SourceAssetConfig {
    pub(super) depot_root: Option<PathBuf>,
    config_file: Option<PathBuf>,
}

impl SourceAssetConfig {
    pub(super) fn set_csgo_path(&mut self, path: PathBuf) -> Result<(), String> {
        let canonical = path.canonicalize().map_err(|error| {
            format!(
                "Depot directory cannot be opened ({}): {error}",
                path.display()
            )
        })?;
        if !canonical.is_dir() {
            return Err(format!(
                "Depot path is not a directory: {}",
                canonical.display()
            ));
        }
        resolve_directory_vpk(&canonical)?;
        self.depot_root = Some(canonical);
        Ok(())
    }

    pub(super) fn save(&self) -> Result<(), String> {
        let path = self
            .config_file
            .as_ref()
            .ok_or_else(|| "config.toml path is unavailable".to_string())?;
        let content = toml::to_string_pretty(&UserConfig {
            depot_path: self.depot_root.clone(),
        })
        .map_err(|error| format!("Could not encode config.toml: {error}"))?;
        std::fs::write(path, content)
            .map_err(|error| format!("Could not write {}: {error}", path.display()))
    }
}

#[derive(Resource, Default, Clone)]
pub(super) struct SourceAssetManager {
    pub(super) mounted_root: Option<PathBuf>,
    index: Option<Arc<VpkDirectoryIndex>>,
}

impl SourceAssetManager {
    pub(super) fn read_asset(&self, path: &str) -> Result<Vec<u8>, String> {
        let index = self
            .index
            .as_ref()
            .ok_or_else(|| "Source depot is not mounted yet".to_string())?;
        read_vpk_entry(index, path)
    }

    pub(super) fn request_map(&self, map_name: &str) -> Result<String, String> {
        let entry_path = map_entry_path(map_name)?;
        let index = self
            .index
            .as_ref()
            .ok_or_else(|| "Source depot is not mounted yet".to_string())?;
        if !index.lookup.contains_key(&entry_path) {
            return Err(format!(
                "Map {map_name} was not found in the mounted VPK directory"
            ));
        }
        if !index
            .validated_bsp_headers
            .iter()
            .any(|name| name == &entry_path)
        {
            return Err(format!("Map {map_name} has no validated BSP header"));
        }
        Ok(format!("Map {map_name} is available"))
    }

    pub(super) fn read_map(&self, map_name: &str) -> Result<Vec<u8>, String> {
        let entry_path = map_entry_path(map_name)?;
        let index = self
            .index
            .as_ref()
            .ok_or_else(|| "Source depot is not mounted yet".to_string())?;
        if !index
            .validated_bsp_headers
            .iter()
            .any(|name| name == &entry_path)
        {
            return Err(format!("Map {map_name} has no validated BSP header"));
        }
        if let Some(path) = index.local_maps.get(&entry_path) {
            return std::fs::read(path)
                .map_err(|error| format!("Could not read map {}: {error}", path.display()));
        }
        read_vpk_entry(index, &entry_path)
    }
}

fn map_entry_path(map_name: &str) -> Result<String, String> {
    if map_name.is_empty() || map_name.contains(['/', '\\', ':']) || map_name.contains("..") {
        return Err("map expects a simple map name such as de_dust2".to_string());
    }
    Ok(format!("maps/{}.bsp", map_name.to_ascii_lowercase()))
}

#[derive(Clone, Debug)]
struct VpkEntry {
    path: String,
    _crc32: u32,
    preload: Vec<u8>,
    archive_index: u16,
    offset: u32,
    length: u32,
}

#[derive(Debug)]
struct VpkDirectoryIndex {
    directory_path: PathBuf,
    version: u32,
    header_size: u64,
    tree_size: u64,
    entries: Vec<VpkEntry>,
    lookup: HashMap<String, usize>,
    validated_bsp_headers: Vec<String>,
    local_maps: HashMap<String, PathBuf>,
}

#[derive(Resource, Default)]
struct VpkIndexing {
    task: Option<Task<Result<VpkDirectoryIndex, String>>>,
    processed_entries: Arc<AtomicUsize>,
    estimated_entries: Arc<AtomicUsize>,
}

#[derive(Resource)]
struct SetupUiState {
    path: String,
    status: String,
    path_focused: bool,
}

#[derive(Resource, Default)]
struct LoaderScreenEntities(Vec<Entity>);

#[derive(Component)]
struct SetupPathField;

#[derive(Component)]
struct SetupPathText;

#[derive(Component)]
struct SetupStatusText;

#[derive(Component)]
struct VerifyDepotButton;

#[derive(Component)]
struct SkipDepotButton;

#[derive(Component)]
struct IndexStatusText;

#[derive(Component)]
struct IndexProgressFill;

#[derive(Deserialize, Serialize)]
struct UserConfig {
    #[serde(default)]
    depot_path: Option<PathBuf>,
}

struct Bootstrap {
    state: AppState,
    config: SourceAssetConfig,
    status: String,
}

impl Bootstrap {
    fn load() -> Self {
        let config_path = match std::env::current_dir() {
            Ok(directory) => directory.join("config.toml"),
            Err(error) => {
                return Self {
                    state: AppState::Setup,
                    config: SourceAssetConfig::default(),
                    status: format!("Could not locate config.toml: {error}"),
                };
            }
        };

        let mut config = SourceAssetConfig {
            config_file: Some(config_path.clone()),
            ..default()
        };
        match std::fs::read_to_string(&config_path) {
            Ok(contents) => match toml::from_str::<UserConfig>(&contents) {
                Ok(user_config) => {
                    if let Some(path) = user_config.depot_path {
                        let entered_path = path.clone();
                        match config.set_csgo_path(path) {
                            Ok(()) => {
                                return Self {
                                    state: AppState::Indexing,
                                    config,
                                    status: format!(
                                        "Found depot configuration at {}. Checking VPK archives...",
                                        entered_path.display()
                                    ),
                                };
                            }
                            Err(error) => {
                                return Self {
                                    state: AppState::Setup,
                                    config: SourceAssetConfig {
                                        depot_root: Some(entered_path),
                                        ..config
                                    },
                                    status: error,
                                };
                            }
                        }
                    }
                }
                Err(error) => {
                    return Self {
                        state: AppState::Setup,
                        config,
                        status: format!("config.toml could not be parsed: {error}"),
                    };
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Self {
                    state: AppState::Setup,
                    config,
                    status: format!("Could not read config.toml: {error}"),
                };
            }
        }
        Self {
            state: AppState::Setup,
            config,
            status: "Укажите путь к папке Steam Depot, содержащей /csgo/pak01_dir.vpk".to_string(),
        }
    }
}

fn spawn_setup_screen(
    mut commands: Commands,
    mut screen_entities: ResMut<LoaderScreenEntities>,
    setup: Res<SetupUiState>,
) {
    let camera = commands.spawn(Camera2dBundle::default()).id();
    let mut root = commands.spawn(NodeBundle {
        style: Style {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            row_gap: Val::Px(18.0),
            ..default()
        },
        background_color: Color::srgb(0.035, 0.045, 0.06).into(),
        ..default()
    });
    root.with_children(|root| {
        root.spawn(TextBundle::from_section(
            "Kisak Strike Rust — Setup",
            TextStyle {
                font_size: 34.0,
                color: Color::WHITE,
                ..default()
            },
        ));
        root.spawn(TextBundle::from_section(
            "Подключать Steam Depot необязательно. Укажите его путь или пропустите для запуска демо-карты.",
            TextStyle {
                font_size: 20.0,
                color: Color::srgb(0.82, 0.86, 0.92),
                ..default()
            },
        ));
        root.spawn((
            ButtonBundle {
                style: Style {
                    width: Val::Percent(72.0),
                    min_height: Val::Px(54.0),
                    padding: UiRect::all(Val::Px(14.0)),
                    align_items: AlignItems::Center,
                    ..default()
                },
                background_color: Color::srgb(0.12, 0.15, 0.2).into(),
                ..default()
            },
            SetupPathField,
        ))
        .with_children(|field| {
            field.spawn((
                TextBundle::from_section(
                    format!("> {}", setup.path),
                    TextStyle {
                        font_size: 18.0,
                        color: Color::WHITE,
                        ..default()
                    },
                ),
                SetupPathText,
            ));
        });
        root.spawn((
            ButtonBundle {
                style: Style {
                    width: Val::Px(230.0),
                    height: Val::Px(52.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                background_color: Color::srgb(0.13, 0.36, 0.24).into(),
                ..default()
            },
            VerifyDepotButton,
        ))
        .with_children(|button| {
            button.spawn(TextBundle::from_section(
                "Verify & Mount",
                TextStyle {
                    font_size: 20.0,
                    color: Color::WHITE,
                    ..default()
                },
            ));
        });
        root.spawn((
            ButtonBundle {
                style: Style {
                    width: Val::Px(320.0),
                    height: Val::Px(52.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                background_color: Color::srgb(0.2, 0.27, 0.39).into(),
                ..default()
            },
            SkipDepotButton,
        ))
        .with_children(|button| {
            button.spawn(TextBundle::from_section(
                "Пропустить — запустить демо-карту",
                TextStyle {
                    font_size: 18.0,
                    color: Color::WHITE,
                    ..default()
                },
            ));
        });
        root.spawn((
            TextBundle::from_section(
                setup.status.clone(),
                TextStyle {
                    font_size: 16.0,
                    color: Color::srgb(0.95, 0.78, 0.42),
                    ..default()
                },
            ),
            SetupStatusText,
        ));
    });
    screen_entities.0.push(camera);
    screen_entities.0.push(root.id());
}

fn handle_setup_input(
    mut keyboard: EventReader<bevy::input::keyboard::KeyboardInput>,
    keys: Res<ButtonInput<KeyCode>>,
    mut fields: Query<&Interaction, (With<SetupPathField>, Changed<Interaction>)>,
    buttons: Query<&Interaction, (With<VerifyDepotButton>, Changed<Interaction>)>,
    skip_buttons: Query<&Interaction, (With<SkipDepotButton>, Changed<Interaction>)>,
    mut setup: ResMut<SetupUiState>,
    mut config: ResMut<SourceAssetConfig>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    if skip_buttons
        .iter()
        .any(|interaction| *interaction == Interaction::Pressed)
    {
        next_state.set(AppState::InGame);
        return;
    }

    for interaction in &mut fields {
        if *interaction == Interaction::Pressed {
            setup.path_focused = true;
        }
    }
    let should_verify = buttons
        .iter()
        .any(|interaction| *interaction == Interaction::Pressed)
        || (setup.path_focused && keys.just_pressed(KeyCode::Enter));

    let control_pressed = keys.pressed(KeyCode::ControlLeft) || keys.pressed(KeyCode::ControlRight);
    let mut paste_handled = false;
    for event in keyboard.read() {
        if !setup.path_focused || event.state != bevy::input::ButtonState::Pressed {
            continue;
        }
        if event.key_code == KeyCode::Backspace {
            setup.path.pop();
        } else if event.key_code == KeyCode::KeyV && control_pressed {
            if !paste_handled {
                paste_handled = true;
                paste_setup_path(&mut setup);
            }
        } else if event.key_code == KeyCode::Space {
            append_setup_space(&mut setup.path);
        } else if let bevy::input::keyboard::Key::Character(character) = &event.logical_key {
            if !control_pressed && character.chars().all(|character| !character.is_control()) {
                setup.path.push_str(character);
            }
        }
    }

    if !should_verify {
        return;
    }
    let entered_path = setup.path.trim().trim_matches('"').trim_matches('\'');
    if entered_path.is_empty() {
        setup.status = "Введите путь к папке Steam Depot.".to_string();
        return;
    }
    match config.set_csgo_path(PathBuf::from(entered_path)) {
        Ok(()) => match config.save() {
            Ok(()) => {
                setup.status = "Путь сохранён. Запускается проверка VPK...".to_string();
                next_state.set(AppState::Indexing);
            }
            Err(error) => setup.status = error,
        },
        Err(error) => setup.status = error,
    }
}

fn paste_setup_path(setup: &mut SetupUiState) {
    match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
        Ok(text) => {
            setup.path.push_str(&sanitize_pasted_path(&text));
            setup.status.clear();
        }
        Err(error) => {
            setup.status = format!("Не удалось вставить путь из буфера обмена: {error}");
        }
    }
}

fn append_setup_space(path: &mut String) {
    path.push(' ');
}

fn sanitize_pasted_path(text: &str) -> String {
    text.chars()
        .filter(|character| !matches!(character, '\r' | '\n' | '\0'))
        .collect()
}

fn update_setup_screen(
    setup: Res<SetupUiState>,
    mut path_text: Query<&mut Text, (With<SetupPathText>, Without<SetupStatusText>)>,
    mut status_text: Query<&mut Text, (With<SetupStatusText>, Without<SetupPathText>)>,
) {
    if !setup.is_changed() {
        return;
    }
    if let Ok(mut text) = path_text.get_single_mut() {
        text.sections[0].value = format!("> {}", setup.path);
    }
    if let Ok(mut text) = status_text.get_single_mut() {
        text.sections[0].value.clone_from(&setup.status);
    }
}

fn begin_vpk_indexing(
    config: Res<SourceAssetConfig>,
    mut indexing: ResMut<VpkIndexing>,
    mut manager: ResMut<SourceAssetManager>,
    mut next_state: ResMut<NextState<AppState>>,
    mut setup: ResMut<SetupUiState>,
) {
    let Some(root) = config.depot_root.clone() else {
        setup.status = "Depot path is not configured.".to_string();
        next_state.set(AppState::Setup);
        return;
    };
    let directory_path = match resolve_directory_vpk(&root) {
        Ok(path) => path,
        Err(error) => {
            setup.status = error;
            next_state.set(AppState::Setup);
            return;
        }
    };
    manager.mounted_root = None;
    manager.index = None;
    let processed = Arc::new(AtomicUsize::new(0));
    let estimate = Arc::new(AtomicUsize::new(1));
    let task_processed = Arc::clone(&processed);
    let task_estimate = Arc::clone(&estimate);
    indexing.processed_entries = processed;
    indexing.estimated_entries = estimate;
    indexing.task =
        Some(AsyncComputeTaskPool::get().spawn(async move {
            index_vpk_directory(&directory_path, task_processed, task_estimate)
        }));
}

fn spawn_indexing_screen(
    mut commands: Commands,
    mut screen_entities: ResMut<LoaderScreenEntities>,
) {
    let camera = commands.spawn(Camera2dBundle::default()).id();
    let mut root = commands.spawn(NodeBundle {
        style: Style {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            justify_content: JustifyContent::Center,
            row_gap: Val::Px(20.0),
            ..default()
        },
        background_color: Color::srgb(0.035, 0.045, 0.06).into(),
        ..default()
    });
    root.with_children(|root| {
        root.spawn((
            TextBundle::from_section(
                "Индексирование архивов Valve...",
                TextStyle {
                    font_size: 26.0,
                    color: Color::WHITE,
                    ..default()
                },
            ),
            IndexStatusText,
        ));
        root.spawn(NodeBundle {
            style: Style {
                width: Val::Percent(65.0),
                height: Val::Px(22.0),
                ..default()
            },
            background_color: Color::srgb(0.12, 0.14, 0.18).into(),
            ..default()
        })
        .with_children(|bar| {
            bar.spawn((
                NodeBundle {
                    style: Style {
                        width: Val::Percent(0.0),
                        height: Val::Percent(100.0),
                        ..default()
                    },
                    background_color: Color::srgb(0.25, 0.72, 0.44).into(),
                    ..default()
                },
                IndexProgressFill,
            ));
        });
    });
    screen_entities.0.push(camera);
    screen_entities.0.push(root.id());
}

fn poll_vpk_indexing(
    mut indexing: ResMut<VpkIndexing>,
    mut manager: ResMut<SourceAssetManager>,
    mut setup: ResMut<SetupUiState>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    let result = indexing
        .task
        .as_mut()
        .and_then(|task| future::block_on(future::poll_once(task)));
    let Some(result) = result else {
        return;
    };
    indexing.task = None;
    match result {
        Ok(index) => {
            let file_count = index.entries.len();
            let bsp_count = index.validated_bsp_headers.len();
            let root = index
                .directory_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf();
            info!(
                "Mounted Source VPK v{} from {}: {file_count} entries indexed, {bsp_count} BSP headers validated",
                index.version,
                index.directory_path.display()
            );
            setup.status = format!(
                "Mounted VPK v{}: {file_count} entries indexed; {bsp_count} BSP headers validated.",
                index.version
            );
            manager.mounted_root = Some(root);
            manager.index = Some(Arc::new(index));
            next_state.set(AppState::InGame);
        }
        Err(error) => {
            setup.status = format!("Depot verification failed: {error}");
            next_state.set(AppState::Setup);
        }
    }
}

fn update_indexing_screen(
    indexing: Res<VpkIndexing>,
    mut status: Query<&mut Text, With<IndexStatusText>>,
    mut fill: Query<&mut Style, With<IndexProgressFill>>,
) {
    let done = indexing.processed_entries.load(Ordering::Relaxed);
    let total = indexing.estimated_entries.load(Ordering::Relaxed).max(1);
    let percent = (done as f32 / total as f32 * 100.0).clamp(0.0, 99.0);
    if let Ok(mut text) = status.get_single_mut() {
        text.sections[0].value = format!("Индексирование VPK: {done} / ~{total} файлов...");
    }
    if let Ok(mut style) = fill.get_single_mut() {
        style.width = Val::Percent(percent);
    }
}

fn set_game_cursor(mut windows: Query<&mut Window, With<PrimaryWindow>>) {
    if let Ok(mut window) = windows.get_single_mut() {
        window.cursor.grab_mode = bevy::window::CursorGrabMode::Locked;
        window.cursor.visible = false;
    }
}

fn despawn_loader_screen(
    mut commands: Commands,
    mut screen_entities: ResMut<LoaderScreenEntities>,
) {
    for entity in screen_entities.0.drain(..) {
        commands.entity(entity).despawn_recursive();
    }
}

fn resolve_directory_vpk(root: &Path) -> Result<PathBuf, String> {
    let candidates = [
        root.join("csgo").join("pak01_dir.vpk"),
        root.join("pak01_dir.vpk"),
    ];
    candidates
        .into_iter()
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| format!("Could not find csgo/pak01_dir.vpk under {}", root.display()))
}

fn index_vpk_directory(
    directory_path: &Path,
    processed: Arc<AtomicUsize>,
    estimated: Arc<AtomicUsize>,
) -> Result<VpkDirectoryIndex, String> {
    // 1. Открываем и парсим сам VPK файл (pak01_dir.vpk) для звуков, моделей и оружия
    let mut file = File::open(directory_path)
        .map_err(|error| format!("Could not open {}: {error}", directory_path.display()))?;
    let file_size = file
        .metadata()
        .map_err(|error| format!("Could not inspect VPK metadata: {error}"))?
        .len();
    let mut base_header = [0_u8; 12];
    file.read_exact(&mut base_header)
        .map_err(|error| format!("VPK header is truncated: {error}"))?;
    let magic = u32::from_le_bytes(base_header[0..4].try_into().unwrap());
    if magic != VPK_SIGNATURE {
        return Err(format!("Invalid VPK signature: 0x{magic:08x}"));
    }
    let version = u32::from_le_bytes(base_header[4..8].try_into().unwrap());
    let tree_size = u32::from_le_bytes(base_header[8..12].try_into().unwrap()) as usize;
    if !matches!(version, 1 | 2) {
        return Err(format!(
            "Unsupported VPK version {version}; expected 1 or 2"
        ));
    }

    let (header_size, declared_data_size) = if version == 2 {
        let mut extra = [0_u8; 16];
        file.read_exact(&mut extra)
            .map_err(|error| format!("VPK v2 header is truncated: {error}"))?;
        (
            28_u64,
            Some(u32::from_le_bytes(extra[0..4].try_into().unwrap()) as u64),
        )
    } else {
        (12_u64, None)
    };
    if tree_size > MAX_VPK_TREE_SIZE {
        return Err(format!(
            "VPK directory tree is too large: {tree_size} bytes"
        ));
    }
    let tree_end = header_size
        .checked_add(tree_size as u64)
        .ok_or_else(|| "VPK tree size overflow".to_string())?;
    if tree_end > file_size {
        return Err("VPK directory tree extends beyond the file".to_string());
    }
    let data_size = declared_data_size.unwrap_or(file_size - tree_end);
    if tree_end + data_size > file_size {
        return Err("VPK inline data section extends beyond the file".to_string());
    }
    estimated.store((tree_size / 28).max(1), Ordering::Relaxed);
    let mut tree = vec![0; tree_size];
    file.read_exact(&mut tree)
        .map_err(|error| format!("Could not read VPK directory tree: {error}"))?;

    // Парсим дерево файлов внутри VPK
    let entries = parse_vpk_tree(&tree, data_size, &processed, &estimated)?;
    if entries.is_empty() {
        return Err("VPK directory tree contains no files".to_string());
    }

    // Собираем базовый хэш-мап для быстрого поиска ассетов
    let mut lookup = HashMap::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let _ = lookup.insert(entry.path.to_ascii_lowercase(), index);
        if entry.archive_index != VPK_INLINE_ARCHIVE {
            let archive_path = archive_path_for(directory_path, entry.archive_index);
            let archive_size = archive_path
                .metadata()
                .ok()
                .filter(|metadata| metadata.is_file())
                .map(|metadata| metadata.len())
                .ok_or_else(|| {
                    format!(
                        "VPK entry {} references missing archive {}",
                        entry.path,
                        archive_path.display()
                    )
                })?;
            if entry.offset as u64 + entry.length as u64 > archive_size {
                return Err(format!(
                    "VPK entry {} extends beyond archive {}",
                    entry.path,
                    archive_path.display()
                ));
            }
        }
    }

    let mut index = VpkDirectoryIndex {
        directory_path: directory_path.to_path_buf(),
        version,
        header_size,
        tree_size: tree_size as u64,
        entries,
        lookup,
        validated_bsp_headers: Vec::new(),
        local_maps: HashMap::new(),
    };

    // 2. Ищем встроенные карты внутри самого VPK
    let bsp_entries: Vec<usize> = index
        .entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            let path = entry.path.to_ascii_lowercase();
            path.starts_with("maps/") && path.ends_with(".bsp")
        })
        .map(|(entry_index, _)| entry_index)
        .collect();
    for entry_index in bsp_entries {
        let entry = &index.entries[entry_index];
        if validate_bsp_entry(
            &index.directory_path,
            index.header_size,
            index.tree_size,
            entry,
        )
        .is_ok()
        {
            index
                .validated_bsp_headers
                .push(entry.path.to_ascii_lowercase());
        }
    }

    // 3. ДОПОЛНИТЕЛЬНО сканируем реальную физическую папку csgo/maps на диске
    let csgo_root = directory_path.parent().unwrap_or_else(|| Path::new("."));
    let real_maps_dir = csgo_root.join("maps");
    if real_maps_dir.is_dir() {
        if let Ok(dir_entries) = std::fs::read_dir(&real_maps_dir) {
            for entry in dir_entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("bsp") {
                    if let Some(file_name) = path.file_name().and_then(|s| s.to_str()) {
                        let virtual_path = format!("maps/{}", file_name.to_ascii_lowercase());

                        // Если карты еще нет в индексе, проверяем и добавляем её
                        if !index.lookup.contains_key(&virtual_path) {
                            if let Ok(mut file) = File::open(&path) {
                                let mut header = vec![0; BSP_HEADER_SIZE];
                                if file.read_exact(&mut header).is_ok() {
                                    if let Ok(file_size) = path.metadata().map(|m| m.len()) {
                                        if validate_bsp_header(&header, file_size).is_ok() {
                                            // Создаем фейковую VpkEntry, чтобы логика request_map и lookup работали без ошибок
                                            let fake_index = index.entries.len();
                                            index.entries.push(VpkEntry {
                                                path: virtual_path.clone(),
                                                _crc32: 0,
                                                preload: Vec::new(),
                                                archive_index: VPK_INLINE_ARCHIVE,
                                                offset: 0,
                                                length: 0,
                                            });
                                            index.lookup.insert(virtual_path.clone(), fake_index);
                                            index.validated_bsp_headers.push(virtual_path.clone());
                                            index.local_maps.insert(virtual_path, path);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if index.validated_bsp_headers.is_empty() {
        return Err("No valid maps (*.bsp) found in VPK or local directory.".to_string());
    }
    Ok(index)
}

fn parse_vpk_tree(
    tree: &[u8],
    inline_data_size: u64,
    processed: &AtomicUsize,
    estimated: &AtomicUsize,
) -> Result<Vec<VpkEntry>, String> {
    let mut cursor = 0;
    let mut entries = Vec::new();
    loop {
        let extension = read_cstring(tree, &mut cursor)?;
        if extension.is_empty() {
            break;
        }
        loop {
            let directory = read_cstring(tree, &mut cursor)?;
            if directory.is_empty() {
                break;
            }
            loop {
                let name = read_cstring(tree, &mut cursor)?;
                if name.is_empty() {
                    break;
                }
                if entries.len() >= MAX_VPK_ENTRIES {
                    return Err("VPK directory has too many entries".to_string());
                }
                let record_end = cursor
                    .checked_add(18)
                    .ok_or_else(|| "VPK entry offset overflow".to_string())?;
                if record_end > tree.len() {
                    return Err("VPK entry record is truncated".to_string());
                }
                let crc32 = read_u32(tree, cursor)?;
                let preload_size = read_u16(tree, cursor + 4)? as usize;
                let archive_index = read_u16(tree, cursor + 6)?;
                let offset = read_u32(tree, cursor + 8)?;
                let length = read_u32(tree, cursor + 12)?;
                let terminator = read_u16(tree, cursor + 16)?;
                cursor = record_end;
                if terminator != u16::MAX {
                    return Err(format!("VPK entry {name} has an invalid terminator"));
                }
                let preload_end = cursor
                    .checked_add(preload_size)
                    .ok_or_else(|| "VPK preload offset overflow".to_string())?;
                if preload_end > tree.len() {
                    return Err(format!("VPK preload data for {name} is truncated"));
                }
                let preload = tree[cursor..preload_end].to_vec();
                cursor = preload_end;
                if archive_index == VPK_INLINE_ARCHIVE
                    && offset as u64 + length as u64 > inline_data_size
                {
                    return Err(format!("Inline VPK data for {name} is out of bounds"));
                }

                let mut path = String::new();
                if directory != " " {
                    if directory.starts_with('/')
                        || directory.contains('\\')
                        || directory
                            .split('/')
                            .any(|component| matches!(component, "." | ".."))
                    {
                        return Err(format!("Unsafe VPK directory path: {directory}"));
                    }
                    path.push_str(&directory);
                    path.push('/');
                }
                if name.contains(['/', '\\']) || name == "." || name == ".." {
                    return Err(format!("Unsafe VPK filename: {name}"));
                }
                path.push_str(&name);
                if extension != " " {
                    if extension.contains(['/', '\\', '.']) || extension == ".." {
                        return Err(format!("Unsafe VPK extension: {extension}"));
                    }
                    path.push('.');
                    path.push_str(&extension);
                }
                entries.push(VpkEntry {
                    path,
                    _crc32: crc32,
                    preload,
                    archive_index,
                    offset,
                    length,
                });
                processed.store(entries.len(), Ordering::Relaxed);
                if entries.len() > estimated.load(Ordering::Relaxed) {
                    estimated.store(entries.len(), Ordering::Relaxed);
                }
            }
        }
    }
    if cursor != tree.len() {
        return Err("VPK directory tree contains trailing or malformed bytes".to_string());
    }
    Ok(entries)
}

fn read_cstring(bytes: &[u8], cursor: &mut usize) -> Result<String, String> {
    if *cursor >= bytes.len() {
        return Err("VPK directory tree ended before its terminator".to_string());
    }
    let remaining = &bytes[*cursor..];
    let length = remaining
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(|| "VPK directory tree contains an unterminated string".to_string())?;
    let value = std::str::from_utf8(&remaining[..length])
        .map_err(|_| "VPK directory tree contains a non-UTF-8 path".to_string())?
        .to_string();
    *cursor += length + 1;
    Ok(value)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let data = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| "VPK u16 field is truncated".to_string())?;
    Ok(u16::from_le_bytes(data.try_into().unwrap()))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let data = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| "VPK u32 field is truncated".to_string())?;
    Ok(u32::from_le_bytes(data.try_into().unwrap()))
}

fn archive_path_for(directory_path: &Path, archive_index: u16) -> PathBuf {
    let stem = directory_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("pak01_dir.vpk")
        .strip_suffix("_dir.vpk")
        .unwrap_or("pak01");
    directory_path.with_file_name(format!("{stem}_{archive_index:03}.vpk"))
}

fn read_vpk_entry(index: &VpkDirectoryIndex, name: &str) -> Result<Vec<u8>, String> {
    let key = name.replace('\\', "/").to_ascii_lowercase();
    let entry_index = *index
        .lookup
        .get(&key)
        .ok_or_else(|| format!("Asset not found in VPK: {name}"))?;
    let entry = &index.entries[entry_index];
    let mut result = entry.preload.clone();
    if entry.length > 0 {
        let data_path = if entry.archive_index == VPK_INLINE_ARCHIVE {
            index.directory_path.clone()
        } else {
            archive_path_for(&index.directory_path, entry.archive_index)
        };
        let offset = if entry.archive_index == VPK_INLINE_ARCHIVE {
            index.header_size + index.tree_size + entry.offset as u64
        } else {
            entry.offset as u64
        };
        let mut file = File::open(&data_path)
            .map_err(|error| format!("Could not open {}: {error}", data_path.display()))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| format!("Could not seek to {name}: {error}"))?;
        let old_len = result.len();
        result.resize(old_len + entry.length as usize, 0);
        file.read_exact(&mut result[old_len..])
            .map_err(|error| format!("Could not read {name} from VPK: {error}"))?;
    }
    let expected = entry._crc32;
    let actual = crc32(&result);
    if actual != expected {
        return Err(format!(
            "CRC32 mismatch for {name}: expected {expected:08x}, got {actual:08x}"
        ));
    }
    Ok(result)
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (0_u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VtfPixelFormat {
    Dxt1,
    Dxt5,
}

#[derive(Debug)]
pub(super) struct ParsedVtf {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) mip_count: u32,
    pub(super) pixel_format: VtfPixelFormat,
    pub(super) image: Image,
}

pub(super) fn parse_vtf(bytes: &[u8]) -> Result<ParsedVtf, String> {
    if bytes.len() < 65 || &bytes[0..4] != b"VTF\0" {
        return Err("Invalid or truncated VTF header".to_string());
    }
    let major = read_u32(bytes, 4)?;
    let minor = read_u32(bytes, 8)?;
    if major != 7 || minor > 5 {
        return Err(format!("Unsupported VTF version {major}.{minor}"));
    }
    let header_size = read_u32(bytes, 12)? as usize;
    if header_size < 65 || header_size > bytes.len() {
        return Err(format!("Invalid VTF header size {header_size}"));
    }

    let width = read_u16(bytes, 16)? as u32;
    let height = read_u16(bytes, 18)? as u32;
    if width == 0 || height == 0 {
        return Err("VTF dimensions must be non-zero".to_string());
    }
    let flags = read_u32(bytes, 20)?;
    if flags & 0x0000_4000 != 0 {
        return Err("VTF cubemaps are not supported by this 2D texture path".to_string());
    }
    let frames = read_u16(bytes, 24)?;
    if frames != 1 {
        return Err(format!(
            "Animated VTF with {frames} frames is not supported in this phase"
        ));
    }
    let high_format = read_u32(bytes, 52)?;
    let mip_count = bytes[56] as u32;
    if mip_count == 0 || mip_count > 16 {
        return Err(format!("Invalid VTF mipmap count {mip_count}"));
    }
    let depth = if minor >= 2 {
        read_u16(bytes, 63)? as u32
    } else {
        1
    };
    if depth != 1 {
        return Err(format!(
            "3D VTF textures with depth {depth} are not supported"
        ));
    }
    let (pixel_format, texture_format, block_bytes) = match high_format {
        13 => (
            VtfPixelFormat::Dxt1,
            TextureFormat::Bc1RgbaUnormSrgb,
            8_usize,
        ),
        15 => (
            VtfPixelFormat::Dxt5,
            TextureFormat::Bc3RgbaUnormSrgb,
            16_usize,
        ),
        _ => {
            return Err(format!(
                "Unsupported VTF high-resolution image format {high_format}; only DXT1 and DXT5 are supported"
            ));
        }
    };

    let image_offset = vtf_high_resolution_offset(bytes, header_size, minor)?;
    let mut cursor = image_offset;
    let mut mip_data = vec![Vec::new(); mip_count as usize];
    for mip in (0..mip_count).rev() {
        let mip_width = (width >> mip).max(1);
        let mip_height = (height >> mip).max(1);
        let blocks_x = mip_width.div_ceil(4) as usize;
        let blocks_y = mip_height.div_ceil(4) as usize;
        let size = blocks_x
            .checked_mul(blocks_y)
            .and_then(|blocks| blocks.checked_mul(block_bytes))
            .ok_or_else(|| "VTF mip data size overflow".to_string())?;
        let end = cursor
            .checked_add(size)
            .ok_or_else(|| "VTF image offset overflow".to_string())?;
        let data = bytes
            .get(cursor..end)
            .ok_or_else(|| format!("VTF mip level {mip} is truncated"))?;
        mip_data[mip as usize].extend_from_slice(data);
        cursor = end;
    }
    let compressed_data = mip_data.into_iter().flatten().collect();
    let image = Image {
        data: compressed_data,
        texture_descriptor: TextureDescriptor {
            label: Some("source-vtf-texture"),
            size: Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: mip_count,
            sample_count: 1,
            dimension: TextureDimension::D2,
            format: texture_format,
            usage: TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST,
            view_formats: &[],
        },
        asset_usage: RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
        ..default()
    };
    Ok(ParsedVtf {
        width,
        height,
        mip_count,
        pixel_format,
        image,
    })
}

fn vtf_high_resolution_offset(
    bytes: &[u8],
    header_size: usize,
    minor: u32,
) -> Result<usize, String> {
    if minor < 3 {
        let low_format = read_u32(bytes, 57)?;
        let low_width = bytes[61] as u32;
        let low_height = bytes[62] as u32;
        let low_size = match low_format {
            u32::MAX => 0,
            13 | 20 => compressed_size(low_width, low_height, 8)?,
            14 | 15 => compressed_size(low_width, low_height, 16)?,
            _ if low_format <= 21 && low_format != 7 => (low_width * low_height * 4) as usize,
            _ => {
                return Err(format!("Unsupported VTF thumbnail format {low_format}"));
            }
        };
        return header_size
            .checked_add(low_size)
            .filter(|offset| *offset <= bytes.len())
            .ok_or_else(|| "VTF thumbnail extends beyond the file".to_string());
    }

    let resource_count = read_u32(bytes, 68)? as usize;
    if resource_count > 4096 {
        return Err(format!("VTF has too many resources: {resource_count}"));
    }
    let resource_end = header_size
        .checked_add(
            resource_count
                .checked_mul(8)
                .ok_or_else(|| "VTF resource table size overflow".to_string())?,
        )
        .ok_or_else(|| "VTF resource table offset overflow".to_string())?;
    if resource_end > bytes.len() {
        return Err("VTF resource table is truncated".to_string());
    }
    for index in 0..resource_count {
        let offset = header_size + index * 8;
        if bytes[offset..offset + 3] == [0x30, 0, 0] {
            let data_offset = read_u32(bytes, offset + 4)? as usize;
            if data_offset > bytes.len() {
                return Err("VTF high-resolution image resource is out of bounds".to_string());
            }
            return Ok(data_offset);
        }
    }
    Err("VTF resource table has no high-resolution image resource".to_string())
}

fn compressed_size(width: u32, height: u32, bytes_per_block: usize) -> Result<usize, String> {
    (width.div_ceil(4) as usize)
        .checked_mul(height.div_ceil(4) as usize)
        .and_then(|blocks| blocks.checked_mul(bytes_per_block))
        .ok_or_else(|| "VTF compressed image size overflow".to_string())
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct VmtMaterial {
    shader: String,
    pub(super) base_texture: Option<String>,
    pub(super) bump_map: Option<String>,
}

pub(super) fn parse_vmt(text: &str) -> Result<VmtMaterial, String> {
    let tokens = tokenize_key_values(text)?;
    if tokens.len() < 3 || tokens[1] != "{" {
        return Err("VMT must start with a shader name and an opening brace".to_string());
    }
    let mut material = VmtMaterial {
        shader: tokens[0].to_ascii_lowercase(),
        ..default()
    };
    let mut depth = 0_i32;
    let mut index = 1;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "{" => depth += 1,
            "}" => {
                depth -= 1;
                if depth < 0 {
                    return Err("VMT has an unmatched closing brace".to_string());
                }
            }
            key if depth == 1 => {
                let value = tokens
                    .get(index + 1)
                    .ok_or_else(|| format!("VMT key {key} has no value"))?;
                match key.to_ascii_lowercase().as_str() {
                    "$basetexture" => {
                        material.base_texture = Some(normalize_material_path(value)?);
                    }
                    "$bumpmap" => material.bump_map = Some(normalize_material_path(value)?),
                    _ => {}
                }
                index += 1;
            }
            _ => {}
        }
        index += 1;
    }
    if depth != 0 {
        return Err("VMT has unclosed braces".to_string());
    }
    Ok(material)
}

fn normalize_material_path(path: &str) -> Result<String, String> {
    let normalized = path.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.contains(':')
        || normalized
            .split('/')
            .any(|component| matches!(component, "" | "." | ".."))
    {
        return Err(format!("Unsafe VMT texture path: {path}"));
    }
    let without_root = normalized.strip_prefix("materials/").unwrap_or(&normalized);
    let without_extension = if without_root.to_ascii_lowercase().ends_with(".vtf") {
        &without_root[..without_root.len() - 4]
    } else {
        without_root
    };
    Ok(format!(
        "materials/{}.vtf",
        without_extension.to_ascii_lowercase()
    ))
}

pub(super) fn tokenize_key_values(text: &str) -> Result<Vec<String>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            character if character.is_whitespace() => index += 1,
            '/' if chars.get(index + 1) == Some(&'/') => {
                index += 2;
                while index < chars.len() && chars[index] != '\n' {
                    index += 1;
                }
            }
            '{' | '}' => {
                tokens.push(chars[index].to_string());
                index += 1;
            }
            '"' => {
                index += 1;
                let mut value = String::new();
                while index < chars.len() && chars[index] != '"' {
                    if chars[index] == '\\' && chars.get(index + 1) == Some(&'"') {
                        value.push('"');
                        index += 2;
                    } else {
                        value.push(chars[index]);
                        index += 1;
                    }
                }
                if index >= chars.len() {
                    return Err("VMT contains an unterminated quoted string".to_string());
                }
                index += 1;
                tokens.push(value);
            }
            _ => {
                let start = index;
                while index < chars.len()
                    && !chars[index].is_whitespace()
                    && chars[index] != '{'
                    && chars[index] != '}'
                {
                    index += 1;
                }
                tokens.push(chars[start..index].iter().collect());
            }
        }
    }
    Ok(tokens)
}

fn validate_bsp_entry(
    directory_path: &Path,
    header_size: u64,
    tree_size: u64,
    entry: &VpkEntry,
) -> Result<(), String> {
    let entry_size = entry.preload.len() as u64 + entry.length as u64;
    let mut prefix = entry.preload.clone();
    if prefix.len() < BSP_HEADER_SIZE {
        let needed = BSP_HEADER_SIZE - prefix.len();
        if needed as u64 > entry.length as u64 {
            return Err(format!("BSP header is truncated: {}", entry.path));
        }
        let data_path = if entry.archive_index == VPK_INLINE_ARCHIVE {
            directory_path.to_path_buf()
        } else {
            archive_path_for(directory_path, entry.archive_index)
        };
        let data_offset = if entry.archive_index == VPK_INLINE_ARCHIVE {
            header_size + tree_size + entry.offset as u64
        } else {
            entry.offset as u64
        };
        let mut file = File::open(&data_path)
            .map_err(|error| format!("Could not open {}: {error}", data_path.display()))?;
        file.seek(SeekFrom::Start(data_offset))
            .map_err(|error| format!("Could not seek to {}: {error}", entry.path))?;
        let old_len = prefix.len();
        prefix.resize(BSP_HEADER_SIZE, 0);
        file.read_exact(&mut prefix[old_len..])
            .map_err(|error| format!("Could not read BSP header {}: {error}", entry.path))?;
    }
    validate_bsp_header(&prefix, entry_size).map_err(|error| format!("{}: {error}", entry.path))
}

pub(super) fn validate_bsp_header(header: &[u8], file_size: u64) -> Result<(), String> {
    if header.len() < BSP_HEADER_SIZE {
        return Err("header is truncated".to_string());
    }
    if &header[0..4] != b"VBSP" {
        return Err("invalid VBSP signature".to_string());
    }
    let version = read_u32(header, 4)?;
    if !(19..=21).contains(&version) {
        return Err(format!("unsupported Source BSP version {version}"));
    }
    for lump_index in 0..64 {
        let offset = 8 + lump_index * 16;
        let lump_start = i32::from_le_bytes(header[offset..offset + 4].try_into().unwrap());
        let lump_length = i32::from_le_bytes(header[offset + 4..offset + 8].try_into().unwrap());
        if lump_start < 0 || lump_length < 0 {
            return Err(format!("lump {lump_index} has a negative range"));
        }
        if lump_length > 0
            && (lump_start as u64 + lump_length as u64 > file_size
                || (lump_start as usize) < BSP_HEADER_SIZE)
        {
            return Err(format!("lump {lump_index} is outside the BSP file"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_path_input_keeps_spaces_and_cleans_clipboard_line_breaks() {
        let mut path = String::from("C:\\Program Files");
        append_setup_space(&mut path);
        path.push_str("Steam\\steamapps\\common\\CSGO");
        assert_eq!(path, "C:\\Program Files Steam\\steamapps\\common\\CSGO");
        assert_eq!(
            sanitize_pasted_path("\"C:\\Program Files\\CSGO\"\r\n"),
            "\"C:\\Program Files\\CSGO\""
        );
    }

    fn vtf_fixture(high_format: u32, payload_size: usize) -> Vec<u8> {
        let mut bytes = vec![0_u8; 65 + payload_size];
        bytes[0..4].copy_from_slice(b"VTF\0");
        bytes[4..8].copy_from_slice(&7_u32.to_le_bytes());
        bytes[8..12].copy_from_slice(&2_u32.to_le_bytes());
        bytes[12..16].copy_from_slice(&65_u32.to_le_bytes());
        bytes[16..18].copy_from_slice(&4_u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&4_u16.to_le_bytes());
        bytes[24..26].copy_from_slice(&1_u16.to_le_bytes());
        bytes[52..56].copy_from_slice(&high_format.to_le_bytes());
        bytes[56] = 1;
        bytes[57..61].copy_from_slice(&u32::MAX.to_le_bytes());
        bytes[63..65].copy_from_slice(&1_u16.to_le_bytes());
        bytes
    }

    #[test]
    fn vtf_parser_accepts_bc1_and_preserves_compressed_payload() {
        let parsed = parse_vtf(&vtf_fixture(13, 8)).unwrap();
        assert_eq!(parsed.pixel_format, VtfPixelFormat::Dxt1);
        assert_eq!((parsed.width, parsed.height, parsed.mip_count), (4, 4, 1));
        assert_eq!(
            parsed.image.texture_descriptor.format,
            TextureFormat::Bc1RgbaUnormSrgb
        );
        assert_eq!(parsed.image.data.len(), 8);
    }

    #[test]
    fn vtf_parser_accepts_bc3_and_rejects_other_formats() {
        let parsed = parse_vtf(&vtf_fixture(15, 16)).unwrap();
        assert_eq!(parsed.pixel_format, VtfPixelFormat::Dxt5);
        assert_eq!(
            parsed.image.texture_descriptor.format,
            TextureFormat::Bc3RgbaUnormSrgb
        );
        assert_eq!(parsed.image.data.len(), 16);
        assert!(parse_vtf(&vtf_fixture(14, 16)).is_err());
    }

    #[test]
    fn vmt_parser_normalizes_base_texture_and_rejects_traversal() {
        let material = parse_vmt(
            r#""VertexLitGeneric" { "$basetexture" "materials\models\weapons\ak47.vtf" "$bumpmap" "models/weapons/ak47_n" }"#,
        )
        .unwrap();
        assert_eq!(
            material.base_texture.as_deref(),
            Some("materials/models/weapons/ak47.vtf")
        );
        assert_eq!(
            material.bump_map.as_deref(),
            Some("materials/models/weapons/ak47_n.vtf")
        );
        assert!(parse_vmt(r#""VertexLitGeneric" { "$basetexture" "../escape" }"#).is_err());
    }

    fn unique_temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!(
            "kisak-vpk-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn vpk_record(archive_index: u16, offset: u32, length: u32) -> Vec<u8> {
        vpk_record_with_preload(0, archive_index, offset, length, &[])
    }

    fn vpk_record_with_preload(
        crc: u32,
        archive_index: u16,
        offset: u32,
        length: u32,
        preload: &[u8],
    ) -> Vec<u8> {
        let mut record = Vec::new();
        record.extend_from_slice(&crc.to_le_bytes());
        record.extend_from_slice(&(preload.len() as u16).to_le_bytes());
        record.extend_from_slice(&archive_index.to_le_bytes());
        record.extend_from_slice(&offset.to_le_bytes());
        record.extend_from_slice(&length.to_le_bytes());
        record.extend_from_slice(&u16::MAX.to_le_bytes());
        record.extend_from_slice(preload);
        record
    }

    #[test]
    fn source_asset_reader_extracts_inline_and_split_entries_with_preload_crc() {
        let root = unique_temp_dir();
        let csgo = root.join("csgo");
        std::fs::create_dir_all(&csgo).unwrap();
        let directory_path = csgo.join("pak01_dir.vpk");
        let preload = b"pre";
        let payload = b"fix";
        let mut expected = preload.to_vec();
        expected.extend_from_slice(payload);
        let checksum = crc32(&expected);

        for archive_index in [VPK_INLINE_ARCHIVE, 0] {
            let mut tree = Vec::new();
            tree.extend_from_slice(b"txt\0folder\0sample\0");
            tree.extend_from_slice(&vpk_record_with_preload(
                checksum,
                archive_index,
                0,
                payload.len() as u32,
                preload,
            ));
            tree.extend_from_slice(b"\0\0\0");

            let mut archive = Vec::new();
            archive.extend_from_slice(&VPK_SIGNATURE.to_le_bytes());
            archive.extend_from_slice(&1_u32.to_le_bytes());
            archive.extend_from_slice(&(tree.len() as u32).to_le_bytes());
            archive.extend_from_slice(&tree);
            if archive_index == VPK_INLINE_ARCHIVE {
                archive.extend_from_slice(payload);
                std::fs::write(&directory_path, archive).unwrap();
            } else {
                std::fs::write(&directory_path, archive).unwrap();
                std::fs::write(csgo.join("pak01_000.vpk"), payload).unwrap();
            }

            let entries = parse_vpk_tree(
                &tree,
                if archive_index == VPK_INLINE_ARCHIVE {
                    payload.len() as u64
                } else {
                    0
                },
                &AtomicUsize::new(0),
                &AtomicUsize::new(1),
            )
            .unwrap();
            assert_eq!(entries[0].preload, preload);
            let lookup = entries
                .iter()
                .enumerate()
                .map(|(index, entry)| (entry.path.clone(), index))
                .collect();
            let index = VpkDirectoryIndex {
                directory_path: directory_path.clone(),
                version: 1,
                header_size: 12,
                tree_size: tree.len() as u64,
                entries,
                lookup,
                validated_bsp_headers: Vec::new(),
                local_maps: HashMap::new(),
            };
            let manager = SourceAssetManager {
                mounted_root: Some(csgo.clone()),
                index: Some(Arc::new(index)),
            };
            assert_eq!(
                manager
                    .read_asset("folder/sample.txt")
                    .unwrap_or_else(|error| { panic!("archive index {archive_index}: {error}") }),
                expected
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn depot_path_accepts_root_or_csgo_directory_and_rejects_missing_vpk() {
        let root = unique_temp_dir();
        let csgo = root.join("csgo");
        std::fs::create_dir_all(&csgo).unwrap();
        std::fs::write(csgo.join("pak01_dir.vpk"), b"test").unwrap();

        assert_eq!(
            resolve_directory_vpk(&root).unwrap(),
            csgo.join("pak01_dir.vpk")
        );
        assert_eq!(
            resolve_directory_vpk(&csgo).unwrap(),
            csgo.join("pak01_dir.vpk")
        );
        assert!(resolve_directory_vpk(&root.join("missing")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn vpk_tree_indexes_paths_and_checks_inline_ranges() {
        let mut tree = Vec::new();
        tree.extend_from_slice(b"bsp\0maps\0de_dust2\0");
        tree.extend_from_slice(&vpk_record(VPK_INLINE_ARCHIVE, 0, 4));
        tree.extend_from_slice(b"\0\0\0");

        let processed = AtomicUsize::new(0);
        let estimated = AtomicUsize::new(1);
        let entries = parse_vpk_tree(&tree, 4, &processed, &estimated).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "maps/de_dust2.bsp");
        assert_eq!(entries[0]._crc32, 0);
        assert_eq!(processed.load(Ordering::Relaxed), 1);

        assert!(parse_vpk_tree(&tree, 3, &processed, &estimated).is_err());
    }

    #[test]
    fn bsp_header_validation_checks_version_and_lump_bounds() {
        let mut header = vec![0_u8; BSP_HEADER_SIZE];
        header[0..4].copy_from_slice(b"VBSP");
        header[4..8].copy_from_slice(&20_u32.to_le_bytes());
        let lump = 8 + 3 * 16;
        header[lump..lump + 4].copy_from_slice(&(BSP_HEADER_SIZE as i32).to_le_bytes());
        header[lump + 4..lump + 8].copy_from_slice(&16_i32.to_le_bytes());
        assert!(validate_bsp_header(&header, 1100).is_ok());
        assert!(validate_bsp_header(&header, 1040).is_err());
        header[4..8].copy_from_slice(&42_u32.to_le_bytes());
        assert!(validate_bsp_header(&header, 1100).is_err());
    }

    #[test]
    fn directory_vpk_index_checks_embedded_bsp_header_and_map_lookup() {
        let root = unique_temp_dir();
        std::fs::create_dir_all(root.join("csgo")).unwrap();
        let directory_path = root.join("csgo").join("pak01_dir.vpk");

        let mut bsp = vec![0_u8; BSP_HEADER_SIZE];
        bsp[0..4].copy_from_slice(b"VBSP");
        bsp[4..8].copy_from_slice(&20_u32.to_le_bytes());
        let mut tree = Vec::new();
        tree.extend_from_slice(b"bsp\0maps\0de_dust2\0");
        tree.extend_from_slice(&vpk_record_with_preload(
            crc32(&bsp),
            VPK_INLINE_ARCHIVE,
            0,
            bsp.len() as u32,
            &[],
        ));
        tree.extend_from_slice(b"\0\0\0");

        let mut archive = Vec::new();
        archive.extend_from_slice(&VPK_SIGNATURE.to_le_bytes());
        archive.extend_from_slice(&1_u32.to_le_bytes());
        archive.extend_from_slice(&(tree.len() as u32).to_le_bytes());
        archive.extend_from_slice(&tree);
        archive.extend_from_slice(&bsp);
        std::fs::write(&directory_path, archive).unwrap();

        let index = index_vpk_directory(
            &directory_path,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(1)),
        )
        .unwrap();
        assert_eq!(index.version, 1);
        assert_eq!(index.entries.len(), 1);
        assert_eq!(
            index.validated_bsp_headers,
            vec!["maps/de_dust2.bsp".to_string()]
        );
        let manager = SourceAssetManager {
            mounted_root: Some(root.clone()),
            index: Some(Arc::new(index)),
        };
        assert!(
            manager
                .request_map("de_dust2")
                .unwrap()
                .contains("is available")
        );
        assert_eq!(manager.read_map("de_dust2").unwrap(), bsp);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn vpk_v2_directory_index_reads_bsp_from_split_archive() {
        let root = unique_temp_dir();
        let csgo = root.join("csgo");
        std::fs::create_dir_all(&csgo).unwrap();
        let directory_path = csgo.join("pak01_dir.vpk");
        let chunk_path = csgo.join("pak01_000.vpk");

        let mut bsp = vec![0_u8; BSP_HEADER_SIZE];
        bsp[0..4].copy_from_slice(b"VBSP");
        bsp[4..8].copy_from_slice(&21_u32.to_le_bytes());
        std::fs::write(&chunk_path, &bsp).unwrap();

        let mut tree = Vec::new();
        tree.extend_from_slice(b"bsp\0maps\0de_dust2\0");
        tree.extend_from_slice(&vpk_record(0, 0, bsp.len() as u32));
        tree.extend_from_slice(b"\0\0\0");
        let mut directory_archive = Vec::new();
        directory_archive.extend_from_slice(&VPK_SIGNATURE.to_le_bytes());
        directory_archive.extend_from_slice(&2_u32.to_le_bytes());
        directory_archive.extend_from_slice(&(tree.len() as u32).to_le_bytes());
        directory_archive.extend_from_slice(&[0; 16]);
        directory_archive.extend_from_slice(&tree);
        std::fs::write(&directory_path, directory_archive).unwrap();

        let index = index_vpk_directory(
            &directory_path,
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(1)),
        )
        .unwrap();
        assert_eq!(index.version, 2);
        assert_eq!(
            index.validated_bsp_headers,
            vec!["maps/de_dust2.bsp".to_string()]
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manager_rejects_map_paths_that_could_escape_the_map_namespace() {
        let manager = SourceAssetManager::default();
        assert!(manager.request_map("de_dust2").is_err());
        assert!(manager.request_map("../de_dust2").is_err());
    }

    #[test]
    fn map_reader_loads_a_map_indexed_from_the_local_maps_directory() {
        let root = unique_temp_dir();
        std::fs::create_dir_all(&root).unwrap();
        let map_path = root.join("de_dust2.bsp");
        let bytes = b"validated local map".to_vec();
        std::fs::write(&map_path, &bytes).unwrap();
        let entry_path = "maps/de_dust2.bsp".to_string();
        let index = VpkDirectoryIndex {
            directory_path: root.join("pak01_dir.vpk"),
            version: 1,
            header_size: 12,
            tree_size: 0,
            entries: Vec::new(),
            lookup: HashMap::from([(entry_path.clone(), 0)]),
            validated_bsp_headers: vec![entry_path.clone()],
            local_maps: HashMap::from([(entry_path, map_path)]),
        };
        let manager = SourceAssetManager {
            mounted_root: Some(root.clone()),
            index: Some(Arc::new(index)),
        };

        assert_eq!(manager.read_map("de_dust2").unwrap(), bytes);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_serializes_a_local_depot_path() {
        let config = toml::to_string(&UserConfig {
            depot_path: Some(PathBuf::from(
                r"C:\Steam\steamapps\common\Counter-Strike Global Offensive",
            )),
        })
        .unwrap();
        let decoded: UserConfig = toml::from_str(&config).unwrap();
        assert_eq!(
            decoded.depot_path,
            Some(PathBuf::from(
                r"C:\Steam\steamapps\common\Counter-Strike Global Offensive"
            ))
        );
    }
}
