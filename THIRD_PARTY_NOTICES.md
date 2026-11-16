# Third-party notices

## wgpu-hal

`third_party/wgpu-hal` is [wgpu-hal](https://github.com/gfx-rs/wgpu) 30.0.1 by the gfx-rs
developers, with upstream fixes applied
([gfx-rs/wgpu#10124](https://github.com/gfx-rs/wgpu/pull/10124),
[gfx-rs/wgpu#10714](https://github.com/gfx-rs/wgpu/pull/10714)). It is licensed under MIT or
Apache-2.0; both licenses are in that folder (`LICENSE.MIT`, `LICENSE.APACHE`).

## naga

`third_party/naga` is [naga](https://github.com/gfx-rs/wgpu) 30.0.1 by the gfx-rs developers, with
fixes to its GLSL frontend that shader packs need, each applied in the commit that needed it. It is licensed under MIT or Apache-2.0; both licenses
are in that folder (`LICENSE.MIT`, `LICENSE.APACHE`).

## winit

`third_party/winit` is [winit](https://github.com/rust-windowing/winit) 0.30.13 by the winit
developers, with one fix to its web (wasm) event loop: an event fired from inside an event
handler re-entered `handle_event` and panicked with `RefCell already borrowed`, which unwound
the frame loop and left a black canvas. Re-entrant events are queued instead, as the crate
already does for events that arrive before the runner exists. It is licensed under Apache-2.0;
the licence is in that folder (`LICENSE`).

## Minecraft Miniature Shader

The bundled pack in `shaderpacks/Miniature/` (which the web build packages too) is the unmodified upstream
pack, run through Horizon's shader-pack translator. The lighting and atmosphere model in
`crates/horizon-client-render/src/visual/miniature.rs` and the Minecraft Miniature Shader section of
`crates/horizon-client-render/src/terrain.wgsl` is a Rust/WGSL port of
[Minecraft Miniature Shader](https://github.com/mateuskreuch/minecraft-miniature-shader) by Mateus A.
Kreuch, at commit [`bdb6e00`](https://github.com/mateuskreuch/minecraft-miniature-shader/tree/bdb6e0020b4318d4e845902cbcc3e3552b0e9274).
The upstream pack is under the MIT license:

MIT License

Copyright (c) 2022 Mateus A. Kreuch

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
associated documentation files (the "Software"), to deal in the Software without restriction,
including without limitation the rights to use, copy, modify, merge, publish, distribute,
sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial
portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT
NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES
OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

## Pixelify Sans

`crates/horizon-theme/fonts/PixelifySans-*.ttf` is [Pixelify Sans](https://github.com/eifetx/Pixelify-Sans)
by The Pixelify Sans Project Authors, licensed under the SIL Open Font License 1.1
(`crates/horizon-theme/fonts/OFL-PixelifySans.txt`).

## Pixelarticons

`crates/horizon-theme/icons/pixel/*.svg` are icons from
[Pixelarticons](https://github.com/halfmage/pixelarticons) by Gerrit Halfmann, under the MIT
license (`crates/horizon-theme/icons/pixel/LICENSE-pixelarticons.txt`). `star.svg` is Horizon's own, drawn on
the same grid.

## AMD FidelityFX SDK (FSR 3.1 frame generation)

`crates/horizon-client-render/src/gfx/frame_gen/optical_flow.wgsl` and `interpolation.wgsl` are
ports to WGSL of the optical flow and frame interpolation shaders of the
[AMD FidelityFX SDK](https://github.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK) 1.1.4
(`sdk/include/FidelityFX/gpu/{opticalflow,frameinterpolation}`, at commit
[c6efa6bf7f20](https://github.com/GPUOpen-LibrariesAndSDKs/FidelityFX-SDK/tree/c6efa6bf7f2027b3ec94f28578bb5965eabb9e55)),
and `optical_flow.rs` follows the SDK's `sdk/src/components/opticalflow/ffx_opticalflow.cpp`
(its passes, their order and their dispatch sizes). Copyright (C) 2024 Advanced Micro Devices,
Inc., under the MIT license, whose text heads each file. AMD and FidelityFX are AMD's
trademarks; Horizon is not affiliated with AMD.

## Mod loader logos

The launcher shows the logo of the mod loader an instance plays with.

- `launcher/assets/logos/quilt.png` is the Quilt mini icon (`brand/128png/quilt_mini_icon_transparent.png`)
  from [QuiltMC/art](https://github.com/QuiltMC/art), by the Quilt Project, dedicated to the public
  domain under [CC0 1.0](https://creativecommons.org/publicdomain/zero/1.0/).
- `launcher/assets/logos/neoforge.png` is NeoForged's icon (`art/neoforged_icon_16x16.png`) from
  [neoforged/.github](https://github.com/neoforged/.github/tree/main/art), © the NeoForged team,
  created by Ridanisaurus, under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). It is
  unmodified (the launcher scales it up by whole pixels).
- Fabric's and Forge's icons are not shipped: neither publishes a license for its logo. The launcher
  fetches them from their own sites when an instance needs them
  ([fabric-loader's icon](https://github.com/FabricMC/fabric-loader/blob/master/src/main/resources/assets/fabricloader/icon.png),
  [files.minecraftforge.net's](https://files.minecraftforge.net/)).

Each logo belongs to its project. Showing it says which loader an instance runs; it does not mean
the project endorses Horizon.

## Sign in with Microsoft

The launcher's sign-in buttons follow Microsoft's published branding guidelines for apps that sign in
with Microsoft accounts
([Sign in with Microsoft branding](https://learn.microsoft.com/en-us/entra/identity-platform/howto-add-branding-in-apps)).
The four-square Microsoft symbol in `launcher/src/microsoft.rs` reproduces the squares, positions and
colours of Microsoft's official `ms-symbollockup_mssymbol_19.svg` from that page, unmodified. Microsoft
and its logo are trademarks of the Microsoft group of companies; Horizon is not endorsed by Microsoft.
