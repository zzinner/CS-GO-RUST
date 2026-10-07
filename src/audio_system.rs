use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};

use bevy::{
    audio::{AudioBundle, AudioSource, PlaybackSettings, Volume},
    prelude::*,
    tasks::{AsyncComputeTaskPool, Task, futures_lite::future},
};

use crate::{
    assets_loader::{AppState, SourceAssetManager, tokenize_key_values},
    environment::SurfaceType,
    weapon::WeaponSystems,
};

pub(super) struct AudioSystemPlugin;

impl Plugin for AudioSystemPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SoundBank>()
            .add_event::<SoundCue>()
            .configure_sets(Update, AudioSystemSet.after(WeaponSystems))
            .add_systems(OnEnter(AppState::InGame), begin_sound_manifest_load)
            .add_systems(
                Update,
                (
                    poll_sound_manifest,
                    poll_sound_asset_loads,
                    collect_and_play_sound_cues,
                )
                    .chain()
                    .in_set(AudioSystemSet)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct AudioSystemSet;

#[derive(Event, Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SoundCue {
    WeaponFire,
    Headshot,
    Footstep(SurfaceType),
}

#[derive(Resource, Default)]
struct SoundBank {
    manager: Option<SourceAssetManager>,
    manifest_task: Option<Task<Result<HashMap<String, SoundDefinition>, String>>>,
    definitions: HashMap<String, SoundDefinition>,
    pending: HashMap<String, Task<Result<Vec<Vec<u8>>, String>>>,
    loaded: HashMap<String, LoadedSoundEvent>,
    queued: VecDeque<SoundCue>,
    missing: HashSet<String>,
    random_state: u64,
    manifest_ready: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct SoundDefinition {
    samples: Vec<String>,
    volume: (f32, f32),
    pitch: (f32, f32),
}

struct LoadedSoundEvent {
    samples: Vec<Handle<AudioSource>>,
    volume: (f32, f32),
    pitch: (f32, f32),
}

fn begin_sound_manifest_load(manager: Res<SourceAssetManager>, mut bank: ResMut<SoundBank>) {
    if manager.mounted_root.is_none() {
        warn!("Sound scripts were not loaded because no Source depot is mounted");
        return;
    }
    let manager = manager.clone();
    bank.manager = Some(manager.clone());
    bank.manifest_task =
        Some(AsyncComputeTaskPool::get().spawn(async move { load_sound_definitions(&manager) }));
}

fn poll_sound_manifest(mut bank: ResMut<SoundBank>) {
    let Some(task) = bank.manifest_task.as_mut() else {
        return;
    };
    let Some(result) = future::block_on(future::poll_once(task)) else {
        return;
    };
    bank.manifest_task = None;
    bank.manifest_ready = true;
    match result {
        Ok(definitions) => {
            info!("Indexed {} Valve sound-script events", definitions.len());
            bank.definitions = definitions;
        }
        Err(error) => error!("Could not load Valve sound scripts: {error}"),
    }
}

fn poll_sound_asset_loads(
    mut bank: ResMut<SoundBank>,
    mut audio_sources: ResMut<Assets<AudioSource>>,
) {
    let completed: Vec<_> = bank
        .pending
        .iter_mut()
        .filter_map(|(name, task)| {
            future::block_on(future::poll_once(task)).map(|result| (name.clone(), result))
        })
        .collect();

    for (name, result) in completed {
        bank.pending.remove(&name);
        match result {
            Ok(decoded_files) => {
                let Some((volume, pitch, sample_count)) =
                    bank.definitions.get(&name).map(|definition| {
                        (
                            definition.volume,
                            definition.pitch,
                            definition.samples.len(),
                        )
                    })
                else {
                    error!("Loaded sound event {name} disappeared from its manifest");
                    continue;
                };
                let samples = decoded_files
                    .into_iter()
                    .map(|bytes| {
                        audio_sources.add(AudioSource {
                            bytes: Arc::from(bytes),
                        })
                    })
                    .collect();
                bank.loaded.insert(
                    name.clone(),
                    LoadedSoundEvent {
                        samples,
                        volume,
                        pitch,
                    },
                );
                info!("Cached {sample_count} WAV samples for {name}");
            }
            Err(error_message) => {
                error!("Could not load WAV samples for {name}: {error_message}");
                bank.missing.insert(name.clone());
            }
        }
    }
}

fn collect_and_play_sound_cues(
    mut commands: Commands,
    mut cues: EventReader<SoundCue>,
    mut bank: ResMut<SoundBank>,
) {
    bank.queued.extend(cues.read().copied());
    if !bank.manifest_ready {
        return;
    }

    let mut waiting = VecDeque::new();
    while let Some(cue) = bank.queued.pop_front() {
        let Some(event_name) = resolve_event_name(cue, &bank.definitions) else {
            let label = cue_label(cue);
            if bank.missing.insert(label.to_string()) {
                warn!("No matching Valve soundscript event found for {label}");
            }
            continue;
        };
        if bank.missing.contains(&event_name) {
            continue;
        }
        if let Some((samples, volume_range, pitch_range)) = bank
            .loaded
            .get(&event_name)
            .map(|loaded| (loaded.samples.clone(), loaded.volume, loaded.pitch))
        {
            if samples.is_empty() {
                warn!("Valve soundscript event {event_name} has no loaded WAV samples");
                continue;
            }
            let sample_index = random_index(&mut bank.random_state, samples.len());
            let volume = random_range(&mut bank.random_state, volume_range);
            let pitch = random_range(&mut bank.random_state, pitch_range);
            let source = samples[sample_index].clone();
            commands.spawn(AudioBundle {
                source,
                settings: PlaybackSettings::DESPAWN
                    .with_volume(Volume::new(volume))
                    .with_speed(pitch / 100.0),
            });
            continue;
        }

        if !bank.pending.contains_key(&event_name) {
            let Some(manager) = bank.manager.as_ref().cloned() else {
                error!("Sound event {event_name} requested before a Source depot was mounted");
                continue;
            };
            let Some(definition) = bank.definitions.get(&event_name).cloned() else {
                continue;
            };
            let task_name = event_name.clone();
            bank.pending.insert(
                event_name.clone(),
                AsyncComputeTaskPool::get()
                    .spawn(async move { load_sound_samples(&manager, &task_name, &definition) }),
            );
        }
        waiting.push_back(cue);
    }
    bank.queued = waiting;
}

fn resolve_event_name(
    cue: SoundCue,
    definitions: &HashMap<String, SoundDefinition>,
) -> Option<String> {
    cue_event_candidates(cue)
        .iter()
        .find(|candidate| definitions.contains_key(&candidate.to_ascii_lowercase()))
        .map(|candidate| candidate.to_ascii_lowercase())
}

fn cue_event_candidates(cue: SoundCue) -> &'static [&'static str] {
    match cue {
        SoundCue::WeaponFire => &[
            "Weapon_AK47.Single",
            "Weapon_AK47.Single1",
            "Weapon_AK47.Fire",
        ],
        SoundCue::Headshot => &[
            "Player.DamageHeadshot",
            "Player.Headshot",
            "Player.HitHeadshot",
        ],
        SoundCue::Footstep(SurfaceType::Concrete) => &[
            "Player.Footsteps.Concrete",
            "Player.StepConcrete",
            "Player.Footsteps",
        ],
        SoundCue::Footstep(SurfaceType::Metal) => &[
            "Player.Footsteps.Metal",
            "Player.StepMetal",
            "Player.Footsteps",
        ],
        SoundCue::Footstep(SurfaceType::Wood) => &[
            "Player.Footsteps.Wood",
            "Player.StepWood",
            "Player.Footsteps",
        ],
    }
}

fn cue_label(cue: SoundCue) -> &'static str {
    match cue {
        SoundCue::WeaponFire => "AK-47 fire",
        SoundCue::Headshot => "headshot",
        SoundCue::Footstep(SurfaceType::Concrete) => "concrete footstep",
        SoundCue::Footstep(SurfaceType::Metal) => "metal footstep",
        SoundCue::Footstep(SurfaceType::Wood) => "wood footstep",
    }
}

fn load_sound_definitions(
    manager: &SourceAssetManager,
) -> Result<HashMap<String, SoundDefinition>, String> {
    let manifest_path = "scripts/game_sounds_manifest.txt";
    let manifest_bytes = manager.read_asset(manifest_path)?;
    let manifest_text = std::str::from_utf8(&manifest_bytes)
        .map_err(|error| format!("{manifest_path} is not UTF-8: {error}"))?;
    let manifest_tokens = tokenize_key_values(manifest_text)?;
    let mut script_paths = Vec::new();
    for pair in manifest_tokens.windows(2) {
        if pair[0].eq_ignore_ascii_case("precache_file")
            || pair[0].eq_ignore_ascii_case("preload_file")
        {
            let path = normalize_script_path(&pair[1])?;
            if !script_paths.contains(&path) {
                script_paths.push(path);
            }
        }
    }
    let weapons_path = "scripts/game_sounds_weapons.txt".to_string();
    if !script_paths.contains(&weapons_path) {
        script_paths.push(weapons_path);
    }

    let mut definitions = HashMap::new();
    for script_path in script_paths {
        let script_bytes = manager.read_asset(&script_path)?;
        let script_text = std::str::from_utf8(&script_bytes)
            .map_err(|error| format!("{script_path} is not UTF-8: {error}"))?;
        let parsed = parse_sound_script(script_text)
            .map_err(|error| format!("Could not parse {script_path}: {error}"))?;
        for (name, definition) in parsed {
            definitions
                .entry(name.to_ascii_lowercase())
                .and_modify(|existing: &mut SoundDefinition| {
                    existing.samples.extend(definition.samples.clone());
                    existing.samples.sort();
                    existing.samples.dedup();
                })
                .or_insert(definition);
        }
    }
    Ok(definitions)
}

fn parse_sound_script(text: &str) -> Result<HashMap<String, SoundDefinition>, String> {
    let tokens = tokenize_key_values(text)?;
    let mut cursor = 0;
    let root = parse_key_values_nodes(&tokens, &mut cursor, false)?;
    if cursor != tokens.len() {
        return Err("unexpected trailing soundscript tokens".to_string());
    }

    let mut definitions = HashMap::new();
    collect_sound_events(&root, &mut definitions)?;
    Ok(definitions)
}

#[derive(Debug)]
struct KeyValueNode {
    key: String,
    value: Option<String>,
    children: Vec<KeyValueNode>,
}

fn parse_key_values_nodes(
    tokens: &[String],
    cursor: &mut usize,
    closing_brace: bool,
) -> Result<Vec<KeyValueNode>, String> {
    let mut nodes = Vec::new();
    while *cursor < tokens.len() {
        if tokens[*cursor] == "}" {
            if !closing_brace {
                return Err("soundscript has an unmatched closing brace".to_string());
            }
            *cursor += 1;
            return Ok(nodes);
        }

        let key = tokens[*cursor].clone();
        *cursor += 1;
        let Some(next) = tokens.get(*cursor) else {
            return Err(format!("soundscript key {key} has no value or block"));
        };
        if next == "{" {
            *cursor += 1;
            let children = parse_key_values_nodes(tokens, cursor, true)?;
            nodes.push(KeyValueNode {
                key,
                value: None,
                children,
            });
        } else if next == "}" {
            return Err(format!("soundscript key {key} has no value"));
        } else {
            nodes.push(KeyValueNode {
                key,
                value: Some(next.clone()),
                children: Vec::new(),
            });
            *cursor += 1;
        }
    }
    if closing_brace {
        return Err("soundscript has an unclosed block".to_string());
    }
    Ok(nodes)
}

fn collect_sound_events(
    nodes: &[KeyValueNode],
    definitions: &mut HashMap<String, SoundDefinition>,
) -> Result<(), String> {
    for node in nodes {
        if node.children.is_empty() {
            continue;
        }
        let mut samples = Vec::new();
        collect_wave_paths(node, &mut samples)?;
        if samples.is_empty() {
            collect_sound_events(&node.children, definitions)?;
            continue;
        }
        samples.sort();
        samples.dedup();
        let volume = direct_numeric_range(node, "volume", (1.0, 1.0))?;
        let pitch = direct_numeric_range(node, "pitch", (100.0, 100.0))?;
        if volume.0 < 0.0 || volume.1 > 4.0 || pitch.0 <= 0.0 || pitch.1 > 255.0 {
            return Err(format!(
                "soundscript event {} has out-of-range volume or pitch",
                node.key
            ));
        }
        definitions.insert(
            node.key.to_ascii_lowercase(),
            SoundDefinition {
                samples,
                volume,
                pitch,
            },
        );
    }
    Ok(())
}

fn collect_wave_paths(node: &KeyValueNode, output: &mut Vec<String>) -> Result<(), String> {
    if node.key.eq_ignore_ascii_case("wave") || node.key.to_ascii_lowercase().starts_with("wave") {
        if let Some(value) = &node.value {
            if value.to_ascii_lowercase().ends_with(".wav") {
                output.push(normalize_wave_path(value)?);
            }
        }
    }
    for child in &node.children {
        collect_wave_paths(child, output)?;
    }
    Ok(())
}

fn direct_numeric_range(
    node: &KeyValueNode,
    key: &str,
    default_value: (f32, f32),
) -> Result<(f32, f32), String> {
    let Some(value) = node
        .children
        .iter()
        .find(|child| child.key.eq_ignore_ascii_case(key))
        .and_then(|child| child.value.as_deref())
    else {
        return Ok(default_value);
    };
    let normalized_value = match value.trim().to_ascii_uppercase().as_str() {
        "VOL_NORM" => "1",
        "VOL_LOW" => "0.75",
        "VOL_HIGH" => "1.5",
        "PITCH_NORM" => "100",
        "PITCH_LOW" => "95",
        "PITCH_HIGH" => "120",
        _ => value,
    };
    let numbers = normalized_value
        .split(|character: char| {
            character.is_whitespace() || matches!(character, ',' | ';' | '(' | ')')
        })
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.parse::<f32>()
                .map_err(|error| format!("Invalid {key} range {value:?}: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    match numbers.as_slice() {
        [single] if single.is_finite() => Ok((*single, *single)),
        [minimum, maximum] if minimum.is_finite() && maximum.is_finite() && minimum <= maximum => {
            Ok((*minimum, *maximum))
        }
        _ => Err(format!("Invalid {key} range {value:?}")),
    }
}

fn normalize_script_path(path: &str) -> Result<String, String> {
    let normalized = path.replace('\\', "/");
    let relative = if normalized
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("scripts/"))
    {
        &normalized[8..]
    } else {
        &normalized
    };
    validate_relative_asset_path(relative)?;
    Ok(format!("scripts/{relative}"))
}

fn normalize_wave_path(path: &str) -> Result<String, String> {
    let normalized = path.replace('\\', "/");
    let relative = if normalized
        .get(..6)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("sound/"))
    {
        &normalized[6..]
    } else {
        &normalized
    };
    validate_relative_asset_path(relative)?;
    if !relative.to_ascii_lowercase().ends_with(".wav") {
        return Err(format!("Only WAV sounds are supported: {path}"));
    }
    Ok(format!("sound/{}", relative.to_ascii_lowercase()))
}

fn validate_relative_asset_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.starts_with('/')
        || path.contains(':')
        || path.split('/').any(|part| matches!(part, "" | "." | ".."))
    {
        return Err(format!("Unsafe soundscript asset path: {path}"));
    }
    Ok(())
}

fn load_sound_samples(
    manager: &SourceAssetManager,
    event_name: &str,
    definition: &SoundDefinition,
) -> Result<Vec<Vec<u8>>, String> {
    definition
        .samples
        .iter()
        .map(|path| {
            let bytes = manager
                .read_asset(path)
                .map_err(|error| format!("{event_name}: {error}"))?;
            validate_wav(&bytes).map_err(|error| format!("{path}: {error}"))?;
            Ok(bytes)
        })
        .collect()
}

fn validate_wav(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("invalid RIFF/WAVE header".to_string());
    }
    let riff_size = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let declared_end = riff_size
        .checked_add(8)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| "RIFF size exceeds the WAV data".to_string())?;
    let mut cursor = 12;
    let mut has_format = false;
    let mut has_data = false;
    while cursor + 8 <= declared_end {
        let chunk_size =
            u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        let payload_start = cursor + 8;
        let payload_end = payload_start
            .checked_add(chunk_size)
            .filter(|end| *end <= declared_end)
            .ok_or_else(|| "WAV chunk extends beyond RIFF bounds".to_string())?;
        match &bytes[cursor..cursor + 4] {
            b"fmt " if chunk_size >= 16 => has_format = true,
            b"data" if chunk_size > 0 => has_data = true,
            _ => {}
        }
        cursor = payload_end + (chunk_size & 1);
    }
    if !has_format || !has_data {
        return Err("WAV must contain non-empty fmt and data chunks".to_string());
    }
    Ok(())
}

fn random_index(state: &mut u64, count: usize) -> usize {
    if count <= 1 {
        return 0;
    }
    random_unit(state).mul_add(count as f32, 0.0) as usize % count
}

fn random_range(state: &mut u64, range: (f32, f32)) -> f32 {
    range.0 + (range.1 - range.0) * random_unit(state)
}

fn random_unit(state: &mut u64) -> f32 {
    if *state == 0 {
        *state = 0x9E37_79B9_7F4A_7C15;
    }
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    (*state >> 40) as f32 / (1_u32 << 24) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soundscript_parser_reads_nested_waves_and_parameters() {
        let definitions = parse_sound_script(
            r#"
            "Weapon_AK47.Single"
            {
                "channel" "CHAN_WEAPON"
                "volume" "0.65, 0.85"
                "pitch" "95, 105"
                "rndwave"
                {
                    "wave" "weapons/ak47/ak47-1.wav"
                    "wave" "sound/weapons/ak47/ak47-2.wav"
                }
            }
            "#,
        )
        .unwrap();
        let sound = definitions.get("weapon_ak47.single").unwrap();
        assert_eq!(
            sound.samples,
            vec![
                "sound/weapons/ak47/ak47-1.wav",
                "sound/weapons/ak47/ak47-2.wav"
            ]
        );
        assert_eq!(sound.volume, (0.65, 0.85));
        assert_eq!(sound.pitch, (95.0, 105.0));
    }

    #[test]
    fn soundscript_parser_rejects_unbalanced_blocks_unsafe_paths_and_invalid_ranges() {
        assert!(parse_sound_script(r#""bad" { "wave" "../escape.wav" }"#).is_err());
        assert!(parse_sound_script(r#""bad" { "wave" "good.wav" "#).is_err());
        assert!(parse_sound_script(r#""bad" { "volume" "5" "wave" "good.wav" }"#).is_err());
    }

    #[test]
    fn soundscript_parser_handles_valve_volume_and_pitch_tokens() {
        let definitions = parse_sound_script(
            r#""normal" { "volume" "VOL_NORM" "pitch" "PITCH_LOW" "wave" "sound/test.wav" }"#,
        )
        .unwrap();
        let sound = definitions.get("normal").unwrap();
        assert_eq!(sound.volume, (1.0, 1.0));
        assert_eq!(sound.pitch, (95.0, 95.0));
    }

    #[test]
    fn wav_validation_checks_riff_chunks_and_bounds() {
        let wav = minimal_wav();
        assert!(validate_wav(&wav).is_ok());
        assert!(validate_wav(b"not a wav").is_err());
        let mut truncated = wav;
        truncated[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(validate_wav(&truncated).is_err());
    }

    #[test]
    fn bevy_audio_decoder_decodes_a_valid_pcm_wav() {
        use bevy::audio::Decodable;

        let source = AudioSource {
            bytes: Arc::from(minimal_wav()),
        };
        assert!(source.decoder().count() > 0);
    }

    #[test]
    fn surface_sound_cues_resolve_material_specific_events_before_generic_fallbacks() {
        let definitions = HashMap::from([
            (
                "player.footsteps".to_string(),
                SoundDefinition {
                    samples: vec!["sound/default.wav".to_string()],
                    volume: (1.0, 1.0),
                    pitch: (100.0, 100.0),
                },
            ),
            (
                "player.footsteps.metal".to_string(),
                SoundDefinition {
                    samples: vec!["sound/metal.wav".to_string()],
                    volume: (1.0, 1.0),
                    pitch: (100.0, 100.0),
                },
            ),
        ]);
        assert_eq!(
            resolve_event_name(SoundCue::Footstep(SurfaceType::Metal), &definitions),
            Some("player.footsteps.metal".to_string())
        );
        assert_eq!(
            resolve_event_name(SoundCue::Footstep(SurfaceType::Wood), &definitions),
            Some("player.footsteps".to_string())
        );
    }

    fn minimal_wav() -> Vec<u8> {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&40_u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&44_100_u32.to_le_bytes());
        wav.extend_from_slice(&88_200_u32.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&4_u32.to_le_bytes());
        wav.extend_from_slice(&[0, 0, 0, 0]);
        wav
    }
}
