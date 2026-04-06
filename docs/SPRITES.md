# Sprite System

Pawkit renders the pet as an animated sprite on a transparent `<canvas>`.

## Directory Structure

```
src/assets/sprites/
├── pixel-cat/              # Each sprite set is a folder
│   ├── meta.yaml           # Animation metadata
│   ├── idle.png            # Sprite sheet for idle animation
│   ├── busy.png            # Sprite sheet for busy/working animation
│   ├── success.png         # Sprite sheet for success animation
│   ├── fail.png            # Sprite sheet for fail animation
│   ├── sleep.png           # Sprite sheet for sleeping animation
│   └── walk.png            # Sprite sheet for walk/drag animation
├── pixel-hedgehog/
│   ├── meta.yaml
│   └── ...
```

## meta.yaml Format

Each sprite set has a `meta.yaml` describing its animation frames:

```yaml
name: "Pixel Cat"
author: "Elthen"
license: "Free for commercial/non-commercial use"
source: "https://elthen.itch.io/2d-pixel-art-cat-sprites"
frame_width: 32           # Width of a single frame in pixels
frame_height: 32          # Height of a single frame in pixels
animations:
  idle:
    file: idle.png
    frames: 4              # Number of frames in the sheet
    fps: 6                 # Playback speed
    loop: true
  busy:
    file: busy.png
    frames: 4
    fps: 8
    loop: true
  success:
    file: success.png
    frames: 6
    fps: 8
    loop: false            # Play once then return to idle
  fail:
    file: fail.png
    frames: 4
    fps: 6
    loop: false
  sleep:
    file: sleep.png
    frames: 4
    fps: 3
    loop: true
  walk:
    file: walk.png
    frames: 6
    fps: 8
    loop: true
```

## Animation State Machine

```
                    ┌─────────┐
         ┌─────────│  idle    │◄──────────┐
         │         └────┬─────┘           │
         │              │                 │
    idle_timeout    right-click        animation
    exceeded       → run_action       complete
         │              │                 │
         ▼              ▼                 │
    ┌─────────┐   ┌──────────┐    ┌──────┴────┐
    │  sleep  │   │   busy   │───►│success/fail│
    └─────────┘   └──────────┘    └───────────┘
         │
     any click
         │
         ▼
    ┌─────────┐
    │  idle   │
    └─────────┘
```

Transitions:
- **idle → sleep**: After `idle_timeout` seconds of no interaction
- **idle → busy**: When an action starts executing
- **busy → success**: Action completed with exit code 0
- **busy → fail**: Action completed with non-zero exit code
- **success/fail → idle**: After animation plays once
- **sleep → idle**: On any mouse click
- **any → walk**: While the pet is being dragged

## Recommended Free Sprite Resources

| Name | URL | License | Notes |
|------|-----|---------|-------|
| Elthen's Pixel Art Cat | https://elthen.itch.io/2d-pixel-art-cat-sprites | Free commercial/non-commercial | idle, move, sleep, scared, jump, clean |
| OpenGameArt Hedgehog | https://opengameart.org/content/pixel-art-hedgehog | CC0 | idle (2f), move (6f), sleep (4f) |
| Kenney Animal Pack | https://kenney.nl/assets | CC0 | Various animals |

## Adding a New Sprite Set

1. Create a folder under `src/assets/sprites/` with a descriptive name (lowercase, kebab-case)
2. Place sprite sheet PNGs in the folder (horizontal strip format: frames laid out left-to-right)
3. Create `meta.yaml` following the format above
4. Set the sprite in `config/pet.yaml`: `sprite: "your-sprite-name"`

### Sprite Sheet Format

Each PNG is a horizontal strip of frames:

```
┌──────┬──────┬──────┬──────┐
│ F1   │ F2   │ F3   │ F4   │  ← idle.png (4 frames, 32x32 each)
└──────┴──────┴──────┴──────┘
```

All frames in a sheet must be the same size. The renderer reads frames left-to-right using `frame_width` from meta.yaml.
