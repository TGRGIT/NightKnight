# NightKnight branding

The app icon follows the **Hub System** icon convention (see the `border-patrol`
repo, `branding/README.md`): a 512×512 rounded tile with a single red `#E5484D`
accent, shipping in a **dark** variant (tile `#111519`, white glyph) and a **light**
variant (tile `#F4F5F6`, ink `#0C1014` glyph).

| File | Variant |
|---|---|
| `nightknight-dark.svg` | dark UIs (also used as the web favicon) |
| `nightknight-light.svg` | light UIs |

**Glyph:** a guard's **shield** (the "night-guard" / knight) holding a **glucose
trace**, with the one red accent as the current-reading marker — echoing the SecUnit
visor while saying "CGM" and "watching over you".

To ship alongside the other gated apps, add these (and PNG renders) to
`border-patrol/branding/apps/` as `nightknight-{light,dark}.{svg,png}` and publish via
that repo's `upload-assets.sh`. They will then be served from
`https://assets.cooney.be/apps/nightknight-dark.png` etc. The same mark becomes the
iOS app icon in Phase 2.
