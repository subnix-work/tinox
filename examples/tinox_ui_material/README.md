# Material themes for Tinox-UI

A calm, responsive workspace with rounded cards, clear type, configurable tonal accents,
outlined fields and subtle interaction feedback. The example uses `@TinoxUIApp`
and `@View`; the existing widgets inherit the selected theme automatically.

From the repository root, using a compiler built from this checkout:

```sh
TINOX_UI_THEME=material TINOX_UI_COLOR=blue target/release/tinox run examples/tinox_ui_material/Main.tnx
```

Open <http://localhost:8300/>. Try the preferences tab, save a change, mark the
next step complete, or open the focus dialog. Demo preferences live in the current
browser session on the server and are not persisted to a database.

Restart with `TINOX_UI_THEME=material-dark` for dark surfaces and pastel accents.
`neon` retains the original Tinox appearance and remains the default for existing
apps. Unknown names fail at startup with a clear message.

Choose `TINOX_UI_COLOR=red`, `green`, `blue`, or `purple` for either Material
variant. Purple is the default when the variable is unset or empty. Each preset
provides matching button, text, focus and tonal surface colors for light/dark mode;
error and success messages retain their semantic colors. Unknown values fail
clearly. Color presets require a Material theme; using them with `neon` is an error.

The example overrides `tinox.core:ui` with the workspace source, so it exercises
this implementation before publication. No registry release is needed.

## Selecting a theme in another app

Set `TINOX_UI_THEME=material`, `material-dark`, or `neon` when starting an app.
This works with annotation-driven applications without changing their source.
Custom HTTP shells can call `Assets::shellHtmlWithTheme(wsPort, "material")`
to choose explicitly instead of using the environment.

Fonts stay local by default. Material uses Roboto when installed and otherwise
the system sans-serif font. `TINOX_UI_FONTS=google` optionally loads Roboto (plus
the original theme's fonts) from Google Fonts. The themes require no icon library
or external CSS/JavaScript framework.

Shared semantic `--tinox-*` tokens also style existing `Component::card` content.
Override tokens with your own stylesheet or `withStyle` as needed. Both Material
variants include visible keyboard focus, larger control targets, narrow-screen
card stacking and support for `prefers-reduced-motion`.

The visual direction follows Material 3's semantic color roles and rounded shapes:
<https://m3.material.io/styles/color/roles>.
