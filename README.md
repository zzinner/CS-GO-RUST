## warning: the project is probably poorly written because its vibecoded and i haven't even tested the project past adding the depot loader thing, because im too lazy to download that huge depots but have fun

okay, so this is purely vibecoded rewrite of cs go in rust, i haven't tested it much so i don't even know if its working, but okay ig, alright, so the other part of the readme is also written by ai who knows what was i doing while making this project:


🎮 What Actually Works?

The project is an experimental Rust reimplementation inspired by Kisak-Strike and the Source engine. Its movement, combat, and asset-loading systems are prototypes and have not yet been verified against the original game.
• Source-Inspired Movement Prototype: Implements Source-style friction, ground and air acceleration, jump buffering, slope/step traversal, and Source-unit `sv_gravity`. These systems are still being compared with the C++ implementation and are not yet a verified 1:1 port.
• Counter-Strike-style Crouch: Uses Source's duck-speed penalty/recovery and spline-eased eye-height animation. Collision stays at the standing 72-unit hull while crouching, switches to the 36-unit hull when fully crouched, and returns to standing only when there is room; floor contact is preserved.
• Custom Developer Console (~): Supports a subset of commands, including movement variables, jump binds, and map loading. It is not a full implementation of Valve's console.
• Prototype Ballistics: Includes movement-dependent spread, a prototype recoil curve, hitgroups, and material-based penetration. The recoil table, spread random stream, and complete per-weapon data are not yet ported, so weapon behavior is not yet CS:GO-accurate.
• Speedometer & Tactical Walk: A custom 2D HUD displays movement speed. Base movement and walk/crouch scaling use Source-style values; they have not yet been validated in-game against CS:GO.
• Discrete Hitboxes & Headshots: The visible practice target uses seven box hitgroups (head, chest, stomach, arms, and legs) with damage multipliers. It is a training dummy, not a skeletal player model.

⌨️ Console Cheatsheet & Cheats

Hit the tilde key ~ in-game and try messing with these commands:
• sv_gravity 200 — Set gravity in Source units per second squared (default: 800).
• sv_jump_impulse 301.993 — Set the CS:GO-style upward jump impulse (default: about 302 Source units per second).
• sv_airaccelerate 100 — Turn on high air control limits for effortless Surf/Bhop mechanics.
• sv_maxspeed 320 — Set the global movement speed cap in Source units per second (default: 320; each weapon may impose a lower cap).
• sv_autobunnyhopping 1 — Toggle automatic jump streaming (just hold Space to speed up). Set it to 0 to require manual wheel scrolling.
• sv_enablebunnyhopping 1 — Allow speed to exceed the normal bunnyhop limit. By default, jump takeoff speed is capped at 110% of the current maximum run speed.
• noclip — Disable collisions and gravity to fly through boxes.
• map <name> — Load and view BSP face geometry from maps/<name>.bsp. Map textures, displacements, props, and collision are not imported yet; noclip is enabled for map viewing.
• cl_showpos 1 — Draw live floating coordinate translations and gaze angles on your screen.
• cl_showfps 1 — Toggle the standard performance frame counter.
• clear — Completely wipe the console log output history.

🛠️ Installation & Running

1. Clone this repository down to your machine.
2. Fire up your terminal and type cargo run.
3. On first launch, the Setup screen offers two options:
   • Select Verify & Mount to connect a local Steam Counter-Strike directory containing csgo/pak01_dir.vpk. The game indexes it asynchronously and saves the path to config.toml.
   • Select Пропустить — запустить демо-карту to enter the built-in environment from src/environment.rs immediately; no depot or game files are needed.
   For depot setup, type or paste the absolute path in the field; Ctrl+V pastes from the system clipboard.
To execute the automated engine tests, run: cargo test.

📂 Source Code Layout

• src/main.rs — The application entry point that registers the plugins into the system architecture.
• src/movement.rs — Where all the sliding math, friction loops, edge buffers, wall-sliding, and step traversal live.
• src/weapon.rs — Bullet tracing calculations, spread values, recoil view-punch arrays, and material penetration loss.
• src/target.rs — Training-target entity and box-hitgroup setup.
• src/environment.rs — Spawns the lighting arrays and gray box cubes you walk on.
• src/bsp.rs — Asynchronously reads Source BSP files and renders face geometry. Map textures, displacement surfaces, props, and collision remain unimplemented.
• src/hud.rs — Configures the 2D UI text layouts for rendering your live health, magazine counts, and speedometer.
• src/console.rs — Built-in console systems, keyboard bind maps, and command interpreters.
• src/assets_loader.rs — VPK indexing, asset extraction, and local depot setup.
Everything here is open-source, unlicensed, and built purely for the joy of game dev. Feel free to download, bunnyhop, and play around!
