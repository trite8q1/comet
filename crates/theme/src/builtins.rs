use std::collections::BTreeMap;
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

use crate::{
    AccentRoles, Appearance, Color, SurfaceTreatment, TerminalPalette, ThemeColors, ThemeFamily,
    ThemeRegistry, ThemeSource, ThemeVariant,
};

pub fn builtin_registry() -> &'static ThemeRegistry {
    static REGISTRY: OnceLock<ThemeRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| ThemeRegistry {
        families: vec![family("comet", "Comet", vec![comet_light(), comet_dark()])],
    })
}

fn family(id: &str, name: &str, variants: Vec<ThemeVariant>) -> ThemeFamily {
    ThemeFamily {
        id: id.into(),
        name: name.into(),
        variants,
    }
}

struct Seeds<'a> {
    id: &'a str,
    family_id: &'a str,
    name: &'a str,
    appearance: Appearance,
    treatment: SurfaceTreatment,
    background: &'a str,
    shell: &'a str,
    raised: &'a str,
    card: &'a str,
    text: &'a str,
    muted: &'a str,
    faint: &'a str,
    accent: &'a str,
    danger: &'a str,
    warning: &'a str,
    success: &'a str,
    terminal_background: &'a str,
    ansi: [&'a str; 16],
    syntax: [&'a str; 12],
    source: ThemeSource,
}

fn variant(seed: Seeds<'_>) -> ThemeVariant {
    let background = c(seed.background);
    let shell = c(seed.shell);
    let raised = c(seed.raised);
    let card = c(seed.card);
    let text = c(seed.text);
    let muted = c(seed.muted).ensure_contrast(background, 4.5);
    let faint = c(seed.faint);
    let danger = c(seed.danger);
    let warning = c(seed.warning);
    let success = c(seed.success);
    let accent = AccentRoles::derive(c(seed.accent), seed.appearance, background);
    let dark = seed.appearance.is_dark();
    let border_tone = if dark { Color::WHITE } else { Color::BLACK };
    let solid = if dark {
        Color::rgb(235, 235, 239)
    } else {
        Color::rgb(35, 35, 40)
    };
    let colors = ThemeColors {
        background,
        shell,
        raised,
        card,
        dialog: card.mix(raised, if dark { 0.18 } else { 0.04 }),
        overlay: card.mix(raised, if dark { 0.34 } else { 0.02 }),
        hover: border_tone.with_alpha(if dark { 0.11 } else { 0.06 }),
        active: accent.primary.with_alpha(if dark { 0.18 } else { 0.10 }),
        border: border_tone.with_alpha(if dark { 0.10 } else { 0.12 }),
        border_strong: border_tone.with_alpha(if dark { 0.18 } else { 0.22 }),
        text,
        text_muted: muted,
        text_faint: faint,
        solid,
        on_solid: solid.best_on_color(),
        danger,
        danger_muted: danger.mix(text, 0.28),
        warning,
        warning_muted: warning.mix(text, 0.25),
        success,
        success_muted: success.mix(text, 0.25),
        input: if dark { raised.with_alpha(0.72) } else { card },
        cursor: text.with_alpha(if dark { 0.40 } else { 0.55 }),
        diff_add: success,
        diff_delete: danger,
        diff_hunk: accent.primary.with_alpha(if dark { 0.08 } else { 0.07 }),
    };
    let terminal_background = c(seed.terminal_background);
    let mut variant = ThemeVariant {
        id: seed.id.into(),
        family_id: seed.family_id.into(),
        name: seed.name.into(),
        appearance: seed.appearance,
        recommended_surface_treatment: seed.treatment,
        colors,
        accent,
        syntax: syntax(seed.syntax),
        terminal: TerminalPalette {
            background: terminal_background,
            foreground: text.ensure_contrast(terminal_background, 4.5),
            selection: border_tone.with_alpha(if dark { 0.22 } else { 0.16 }),
            ansi: seed.ansi.map(c),
        },
        source: seed.source,
    };
    // Hash the checked-in resolved definition itself (with the hash field
    // blanked), not merely its source URL. This makes provenance sensitive to
    // curation edits as well as upstream revision changes.
    variant.source.asset_hash.clear();
    let encoded = serde_json::to_vec(&variant).expect("built-in theme serializes");
    variant.source.asset_hash = format!("sha256:{:x}", Sha256::digest(encoded));
    variant
}

fn syntax(colors: [&str; 12]) -> BTreeMap<String, Color> {
    let [
        comment,
        keyword,
        string,
        number,
        type_name,
        function,
        property,
        variable,
        punctuation,
        tag,
        attribute,
        invalid,
    ] = colors.map(c);
    BTreeMap::from([
        ("comment".into(), comment),
        ("keyword".into(), keyword),
        ("string".into(), string),
        ("stringSpecial".into(), attribute),
        ("escape".into(), attribute),
        ("number".into(), number),
        ("boolean".into(), number),
        ("type".into(), type_name),
        ("typeBuiltin".into(), type_name),
        ("constructor".into(), type_name),
        ("function".into(), function),
        ("functionBuiltin".into(), function),
        ("macro".into(), keyword),
        ("property".into(), property),
        ("constant".into(), number),
        ("variable".into(), variable),
        ("variableSpecial".into(), keyword),
        ("parameter".into(), variable),
        ("operator".into(), keyword),
        ("punctuation".into(), punctuation),
        ("tag".into(), tag),
        ("attribute".into(), attribute),
        ("label".into(), function),
        ("embedded".into(), punctuation),
        ("invalid".into(), invalid),
    ])
}

fn source(id: &str, format: &str, url: &str, revision: &str, license: &str) -> ThemeSource {
    ThemeSource {
        format: format.into(),
        url: url.into(),
        revision: revision.into(),
        license: license.into(),
        asset_hash: format!("pending:{id}"),
    }
}

fn c(value: &str) -> Color {
    value.parse().expect("built-in theme colors are valid")
}

const ANSI_DARK: [&str; 16] = [
    "#242424", "#f87171", "#4ade80", "#facc15", "#60a5fa", "#c084fc", "#22d3ee", "#d4d4d8",
    "#52525b", "#fca5a5", "#86efac", "#fde047", "#93c5fd", "#d8b4fe", "#67e8f9", "#fafafa",
];

const ANSI_LIGHT: [&str; 16] = [
    "#1f1f1f", "#dc2626", "#16a34a", "#b45309", "#2563eb", "#9333ea", "#0e7490", "#3f3f46",
    "#71717a", "#b91c1c", "#15803d", "#92400e", "#1d4ed8", "#7e22ce", "#155e75", "#18181b",
];

fn comet_dark() -> ThemeVariant {
    variant(Seeds {
        id: "comet-dark",
        family_id: "comet",
        name: "Comet Dark",
        appearance: Appearance::Dark,
        treatment: SurfaceTreatment::Frosted,
        background: "#060606",
        shell: "#0d0d0d",
        raised: "#343438",
        card: "#0e0e0e",
        text: "#e8e8ea",
        muted: "#a9a9ae",
        faint: "#85858a",
        accent: "#60a5fa",
        danger: "#f87171",
        warning: "#facc15",
        success: "#34d399",
        terminal_background: "#090909",
        ansi: ANSI_DARK,
        syntax: [
            "#92929a", "#8b7cf6", "#34d399", "#facc15", "#c084fc", "#60a5fa", "#f472b6", "#e8e8ea",
            "#a1a1aa", "#f472b6", "#22d3ee", "#f87171",
        ],
        source: source(
            "comet-dark",
            "native",
            "https://github.com/cometsh/comet",
            "d138049",
            "MIT",
        ),
    })
}

fn comet_light() -> ThemeVariant {
    variant(Seeds {
        id: "comet-light",
        family_id: "comet",
        name: "Comet Light",
        appearance: Appearance::Light,
        treatment: SurfaceTreatment::Frosted,
        background: "#ffffff",
        shell: "#f3f3f5",
        raised: "#ededf0",
        card: "#ffffff",
        text: "#303035",
        muted: "#62626a",
        faint: "#797981",
        accent: "#2563eb",
        danger: "#dc2626",
        warning: "#a16207",
        success: "#15803d",
        terminal_background: "#fafafa",
        ansi: ANSI_LIGHT,
        syntax: [
            "#6b7280", "#5b43e8", "#15803d", "#a16207", "#7e22ce", "#2563eb", "#be185d", "#303035",
            "#52525b", "#be185d", "#0e7490", "#b91c1c",
        ],
        source: source(
            "comet-light",
            "native",
            "https://github.com/cometsh/comet",
            "d138049",
            "MIT",
        ),
    })
}
