Benchmark codestreams.

## Second Life defaults

Default textures that every Second Life viewer uses, kept as the asset server or the viewer source delivers them. All were encoded with Kakadu.

| Fixture | Texture | Size | Encoding |
|---|---|---|---|
| `plywood.j2c` | Default object texture `89556747-24cb-43ed-920b-47caed15465f` | 512×512 RGB | 9/7 with MCT, LRCP, PLT, 5 layers |
| `terrain-grass.j2c` | Grass terrain detail `63338ede-0037-c4fd-855b-015d77112fc8` | 512×512 RGB | 9/7 with MCT, LRCP, PLT, 5 layers |
| `water-normal.j2c` | Default water normal map `822ded49-9a6c-f61c-cb89-6df54f42cdf4` | 256×256 RGB | 9/7 with MCT, LRCP, 4 layers |
| `white.j2c` | Blank white `5748decc-f629-461c-9a36-a35a221fe21f` | 32×32 RGB | 9/7 with MCT, 1 layer |
| `profile-picture.j2c` | Viewer skin `default_profile_picture.j2c` | 64×64 RGB | 9/7 with MCT, LRCP, 2 layers |
| `alpha-gradient.j2c` | Viewer skin `alpha_gradient_2d.j2c` | 256×256 gray | 9/7, LRCP, 4 layers |

## Synthetic

`generate.py` writes these from synthetic images. It needs Pillow and OpenJPEG's `opj_compress` on `PATH`:

```sh
uv run --no-project --with pillow python generate.py
```

| Fixture | Content | Encoding |
|---|---|---|
| `gray-alpha.j2c` | 8×8 gray and alpha pattern | 5/3 lossless, 1 layer |
| `gray-4096.j2c` | 4096×4096 constant gray | 5/3 lossless, 5 levels |
| `sculpt-64.j2c` | 64×64 RGB gradients, shaped like a sculpt map | 5/3 lossless with MCT, RLCP, 1 layer, 6 levels |
| `rgb-1024-rlcp.j2c` | 1024×1024 RGB gradient and seeded noise | 9/7 with MCT, RLCP, 5 layers at rates 2048 to 8 |
| `rgba-1024x512-rlcp.j2c` | 1024×512 RGBA gradient and seeded noise | 9/7 with MCT, RLCP, 5 layers at rates 2048 to 8 |
| `rgba-1024-lrcp.j2c` | 1024×1024 RGBA gradient and seeded noise | 9/7 with MCT, LRCP, PLT, 5 layers ending near 700 B, 2,100 B, then ×4 |
| `rgba-2048-lossy.j2c` | 2048×2048 RGBA gradient and seeded noise | 9/7, RPCL, 3 layers, 5 levels |

The RLCP files follow the OpenJPEG upload encoder: layers by image area, rates ×4 per layer down to 8, and MCT for three or more components. `rgba-1024-lrcp.j2c` follows the Kakadu upload encoder's layer byte targets. Pillow writes the files that need no MCT on four components; `opj_compress` writes the rest.
