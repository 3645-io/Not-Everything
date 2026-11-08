# Attribution

Every asset file in the repository: anything under a crate's `assets/` folder or
`docs/img/`. Each is original work by the project's authors, licensed under the project's
MIT licence (`LICENSE`), unless its row says otherwise. Add a row in the same change as the
file; `crates/fx-testkit/tests/attribution.rs` checks that every asset has one and that
every row names a file that exists.

## App icons

Designed by 3645 Team (the approved Glass set of the owner's icon concepts) and built by
`scripts/build-icons.sh`: it draws each design's layers, writes the Icon Composer source
(`AppIcon.icon`) and compiles `AppIcon.icns` and `Assets.car` with Xcode's `actool`.

| File | Author | Source | Licence |
| --- | --- | --- | --- |
| `crates/fx-ui-macos/assets/AppIcon.icon/icon.json` | 3645 Team | Icon Composer document for the Match Row icon | MIT |
| `crates/fx-ui-macos/assets/AppIcon.icon/Assets/match-row-light.png` | 3645 Team | Match Row icon layer, light | MIT |
| `crates/fx-ui-macos/assets/AppIcon.icon/Assets/match-row-dark.png` | 3645 Team | Match Row icon layer, dark | MIT |
| `crates/fx-ui-macos/assets/bundle/AppIcon.icns` | 3645 Team | The app icon, built from `AppIcon.icon` | MIT |
| `crates/fx-ui-macos/assets/bundle/Assets.car` | 3645 Team | The app icon's asset catalog, built from `AppIcon.icon` | MIT |
| `crates/fx-ui-macos/assets/icons/front-stack-light.png` | 3645 Team | Front Stack Dock icon, light | MIT |
| `crates/fx-ui-macos/assets/icons/front-stack-dark.png` | 3645 Team | Front Stack Dock icon, dark | MIT |
| `crates/fx-ui-macos/assets/icons/glow-row-light.png` | 3645 Team | Glow Row Dock icon, light | MIT |
| `crates/fx-ui-macos/assets/icons/glow-row-dark.png` | 3645 Team | Glow Row Dock icon, dark | MIT |
| `crates/fx-ui-macos/assets/icons/lit-slot-light.png` | 3645 Team | Lit Slot Dock icon, light | MIT |
| `crates/fx-ui-macos/assets/icons/lit-slot-dark.png` | 3645 Team | Lit Slot Dock icon, dark | MIT |
| `crates/fx-ui-macos/assets/icons/match-row-light.png` | 3645 Team | Match Row Dock icon, light | MIT |
| `crates/fx-ui-macos/assets/icons/match-row-dark.png` | 3645 Team | Match Row Dock icon, dark | MIT |
| `crates/fx-ui-macos/assets/icons/pressed-lens-light.png` | 3645 Team | Pressed Lens Dock icon, light | MIT |
| `crates/fx-ui-macos/assets/icons/pressed-lens-dark.png` | 3645 Team | Pressed Lens Dock icon, dark | MIT |
| `crates/fx-ui-macos/assets/icons/soft-lens-light.png` | 3645 Team | Soft Lens Dock icon, light | MIT |
| `crates/fx-ui-macos/assets/icons/soft-lens-dark.png` | 3645 Team | Soft Lens Dock icon, dark | MIT |

## Documentation pictures

The plan's diagrams and mock-up, drawn for this project and exported from the plan document
with `docs/PLAN.md`. The mock-up shows sample file names only.

| File | Author | Source | Licence |
| --- | --- | --- | --- |
| `docs/img/architecture.png` | 3645 Team | Architecture diagram in the build plan | MIT |
| `docs/img/mock-ui.png` | 3645 Team | Mock-up of the search window in the build plan | MIT |
