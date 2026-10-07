# Kisak Strike Rust

An independent first-person shooter prototype built with Rust and Bevy. This is an early local
training-range slice, not a complete game or an official Counter-Strike product.

## Current prototype

- First-person mouse look and WASD movement
- Runtime-tunable ground acceleration/friction and gravity, air control, walk/crouch movement, buffered edge-triggered jumps (or optional auto-bhop), noclip, wall sliding, and walkable ramps/steps
- A visible first-person weapon, replaced with the depot AK-47 when its model and materials load, and bright 0.04-second shot tracers
- Movement-dependent spread, an original demo recoil curve, and recovery after firing
- A finite-range hitscan weapon with seven zone hitboxes, per-zone damage, sorted multi-object intersections, material-based penetration/damage loss, a 12-round magazine, automatic fire cooldown, and reload (R)
- Asynchronous Valve soundscript parsing and VPK WAV caching for weapon fire, surface-specific walking/landing footsteps, and lethal headshots
- Screen HUD with health, ammunition/reload state, and a speedometer mapped to 250 source-style units per second at normal run speed
- Developer console (`~`) with runtime ConVars, key binds, `cl_showfps`, `cl_showpos`, and Source depot map lookup
- Depot setup launcher with a TOML path config, VPK directory-tree validation/indexing on Bevy's async compute pool, BSP-header checks, and asynchronous AK-47 MDL/VVD/VTX + VMT/VTF + weapon-script loading
- Escape toggles mouse capture

Run with `cargo run`; run automated checks with `cargo test`.

## Source layout

- `src/main.rs` configures the window and connects the Bevy plugins.
- `src/movement.rs` contains first-person input, mouse capture, movement, swept wall-slide collision, step traversal, and slope support.
- `src/environment.rs` builds the original graybox range, solid geometry, walkable ramp planes, and per-surface friction/jump/penetration response.
- `src/target.rs` contains practice-target health and damage handling.
- `src/weapon.rs` handles hitscan queries, short-lived tracers, spread, view-punch recoil, magazine state, prototype bullet penetration, and asynchronous AK-47 model/material/manifest import.
- `src/audio_system.rs` asynchronously reads Source soundscripts and WAV assets from the mounted VPK, validates/caches samples, and plays event variants through Bevy Audio.
- `src/hud.rs` displays player health, ammunition/reload state, and horizontal movement speed.
- `src/console.rs` owns the developer console, command parser, key binds, and runtime movement ConVars.
- `src/assets_loader.rs` owns Setup/Indexing/InGame app states, the depot config and launcher UI, a Source VPK v1/v2 directory-tree indexer and entry reader, BSP header validation, VTF BC1/BC3 image parsing, and VMT texture-path parsing. BSP geometry extraction/rendering is not implemented yet.

Ramp traversal, material penetration values, recoil, and damage falloff are original prototype
choices. The current graybox collision model uses axis-aligned solid bounds plus explicit walkable
planes for the ramp; it is not a general-purpose triangle-mesh character controller or a claim of
exact parity with another game's physics.

The movement model uses a two-frame jump input buffer and caps horizontal momentum only on an
unbuffered landing; a buffered jump preserves air-strafe speed. Step-up attempts validate overhead
clearance, move horizontally, and snap down to a walkable surface before committing the new position.
Crouch is a camera/collision-height interpolation with prototype duck-fatigue recovery. The HUD is
rendered by Bevy 0.14 UI on the active camera target.

## Implementation boundary

This project does not vendor Kisak-Strike source code or game assets. The reference checkout was
consulted for its README/license notice and selected movement, surface, and weapon behavior. This
Bevy prototype uses independently authored Rust systems and original placeholder geometry; no C++
source fragments or game assets are included. When configured, it reads model, texture, material,
and weapon data directly from the user's external depot at runtime; those files are not extracted
into or redistributed with the repository. The upstream README refers to inherited Source SDK
terms. This statement describes project contents, not a legal opinion or guarantee; review the
applicable terms before using or distributing third-party material.

## Controls

| Input | Action |
| --- | --- |
| W / A / S / D | Move |
| Mouse | Look |
| Left Shift | Walk |
| Left Ctrl | Crouch |
| Space | Jump |
| Hold left mouse button | Fire |
| R | Reload |
| Escape | Release or recapture the mouse |
| `~` | Open/close developer console |

Console examples: `sv_gravity 800`, `sv_autobunnyhopping 1`, `bind "V" "noclip"`,
`bind "MWheelUp" "+jump"`,
`unbind "V"`, `cl_showfps 2`, `cl_showpos 1`, `echo "training range"`, and `clear`.
On first launch, enter the local Steam Depot root (or its `csgo` folder) containing
`csgo/pak01_dir.vpk`, then choose **Verify & Mount**. A successful setup writes the local path to
`config.toml` (ignored by Git) and indexes VPK metadata asynchronously. The game only enters its
graybox scene after the directory archive, referenced split archives, and BSP headers pass checks.
The `path_csgo "<local path>"` console command can change the configured depot; restart the app to
verify and mount the new path.
`map de_dust2` verifies that the indexed BSP exists and has a valid header; loading its geometry and
rendering the map remains a separate follow-up. With an indexed depot, the game asynchronously
attempts to load `models/weapons/v_rif_ak47.mdl` with its matching `.vvd` and `.dx90.vtx`, VMT/VTF
textures (BC1/BC3), and `scripts/weapon_ak47.txt`. The importer targets the MDL versions supported
by the `vformats` parser; unsupported, incomplete, or mismatched asset sets are reported in the
console log and leave the demo weapon visible. Model animation is not implemented. Depot data
remains external and is neither extracted into the repository nor redistributed by this project.
The audio system indexes `scripts/game_sounds_manifest.txt` and
`scripts/game_sounds_weapons.txt`, resolves soundscript WAV paths through the same VPK reader, and
caches requested firing, footstep, and headshot events asynchronously. Missing events or invalid
WAV data are reported in the log.

See [MODDING_PLAN.md](MODDING_PLAN.md) for the implementation route and [MODLOG.md](MODLOG.md)
for progress and decisions.
