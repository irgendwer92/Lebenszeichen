//! Texts for people, in the language of the device.
//!
//! The texts live in Fluent files, `locales/<tag>/lebenszeichen.ftl` at the
//! repository root, and are embedded into the binary. English is the default
//! and the fallback: an id that a translation lacks, or a message that fails
//! to format, is shown in English, and as the bare id if English lacks it too.
//!
//! To add a language, add a variant to [`Language`], list it in
//! [`Language::ALL`] and give it a `locales/<tag>/lebenszeichen.ftl`; the
//! completeness test then checks that the file parses and has the ids of
//! the English one.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource, FluentValue};
use unic_langid::LanguageIdentifier;

/// A language of the texts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Language {
    /// The default and the fallback.
    #[default]
    En,
    De,
}

impl Language {
    /// Every supported language; the single list the tests iterate over.
    pub const ALL: &'static [Language] = &[Language::En, Language::De];

    /// The language tag: `"en"`, `"de"`.
    pub fn tag(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::De => "de",
        }
    }

    /// The language for a tag such as `de`, `de-DE` or a locale name such as
    /// `de_DE.UTF-8`; `None` if it is not supported.
    pub fn from_tag(tag: &str) -> Option<Self> {
        let name = tag.split(['.', '@']).next().unwrap_or_default();
        let id: LanguageIdentifier = name.replace('_', "-").parse().ok()?;
        Self::ALL
            .iter()
            .copied()
            .find(|language| language.tag() == id.language.as_str())
    }

    /// The language of a POSIX locale, given the values of `LC_ALL`,
    /// `LC_MESSAGES` and `LANG`: the first one that is set and not empty
    /// decides. `C`, no value and unknown languages give English.
    pub fn from_locale(
        lc_all: Option<&str>,
        lc_messages: Option<&str>,
        lang: Option<&str>,
    ) -> Self {
        [lc_all, lc_messages, lang]
            .into_iter()
            .flatten()
            .find(|value| !value.is_empty())
            .and_then(Self::from_tag)
            .unwrap_or_default()
    }

    /// The language of the environment of this process.
    pub fn from_env() -> Self {
        let var = |name| std::env::var(name).ok();
        Self::from_locale(
            var("LC_ALL").as_deref(),
            var("LC_MESSAGES").as_deref(),
            var("LANG").as_deref(),
        )
    }

    /// The Fluent source of this language.
    pub fn source(self) -> &'static str {
        match self {
            Self::En => include_str!("../../../locales/en/lebenszeichen.ftl"),
            Self::De => include_str!("../../../locales/de/lebenszeichen.ftl"),
        }
    }

    /// The raw patterns of all texts whose id starts with `prefix`, with
    /// variables as `{ $name }`, for a front end that substitutes them
    /// itself. Ids that this language lacks come from English. Only plain
    /// text, string literals and variables are supported; any other
    /// placeable renders as nothing (a test keeps the `ui-` texts free of
    /// them).
    pub fn raw_texts(self, prefix: &str) -> BTreeMap<String, String> {
        let mut texts = BTreeMap::new();
        for language in [Language::En, self] {
            let Ok(resource) = FluentResource::try_new(language.source().to_owned()) else {
                continue;
            };
            for entry in resource.entries() {
                if let fluent_syntax::ast::Entry::Message(message) = entry
                    && message.id.name.starts_with(prefix)
                    && let Some(pattern) = &message.value
                {
                    texts.insert(message.id.name.to_owned(), render_raw(pattern));
                }
            }
        }
        texts
    }

    /// The translator for this language.
    pub fn texts(self) -> Texts {
        Texts(self)
    }

    fn bundle(self) -> &'static Bundle {
        static EN: OnceLock<Bundle> = OnceLock::new();
        static DE: OnceLock<Bundle> = OnceLock::new();
        let cell = match self {
            Self::En => &EN,
            Self::De => &DE,
        };
        cell.get_or_init(|| new_bundle(self))
    }
}

fn render_raw(pattern: &fluent_syntax::ast::Pattern<&str>) -> String {
    use fluent_syntax::ast::{Expression, InlineExpression, PatternElement};
    let mut text = String::new();
    for element in &pattern.elements {
        match element {
            PatternElement::TextElement { value } => text.push_str(value),
            PatternElement::Placeable { expression } => match expression {
                Expression::Inline(InlineExpression::StringLiteral { value }) => {
                    text.push_str(value);
                }
                Expression::Inline(InlineExpression::VariableReference { id }) => {
                    text.push_str(&format!("{{ ${} }}", id.name));
                }
                _ => {}
            },
        }
    }
    text
}

type Bundle = FluentBundle<FluentResource>;

fn new_bundle(language: Language) -> Bundle {
    let id: LanguageIdentifier = language
        .tag()
        .parse()
        .expect("the tags of the languages are valid");
    // Without the isolation marks around arguments, the output holds only
    // the characters of the texts.
    let mut bundle = FluentBundle::new_concurrent(vec![id]);
    bundle.set_use_isolating(false);
    // Entries that fail to parse are left out; the completeness test fails
    // for them.
    let resource = FluentResource::try_new(language.source().to_owned())
        .unwrap_or_else(|(resource, _)| resource);
    let _ = bundle.add_resource(resource);
    bundle
}

/// An argument of a text.
#[derive(Clone, Copy, Debug)]
pub enum Arg<'a> {
    Text(&'a str),
    /// A number that the text may select a plural form by.
    Count(u64),
}

impl<'a> From<&'a str> for Arg<'a> {
    fn from(text: &'a str) -> Self {
        Self::Text(text)
    }
}

impl<'a> From<&'a String> for Arg<'a> {
    fn from(text: &'a String) -> Self {
        Self::Text(text)
    }
}

macro_rules! count_from {
    ($($number:ty),*) => {$(
        impl From<$number> for Arg<'_> {
            fn from(number: $number) -> Self {
                Self::Count(u64::from(number))
            }
        }
    )*};
}
count_from!(u8, u16, u32, u64);

impl From<usize> for Arg<'_> {
    fn from(number: usize) -> Self {
        Self::Count(number as u64)
    }
}

/// Looks up and formats texts in one language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Texts(Language);

impl Texts {
    pub fn language(&self) -> Language {
        self.0
    }

    /// The text `id` with `args`. Falls back to English, then to the id.
    pub fn text(&self, id: &str, args: &[(&str, Arg<'_>)]) -> String {
        let mut fluent = FluentArgs::new();
        for (name, arg) in args {
            match arg {
                Arg::Text(text) => fluent.set(*name, FluentValue::from(*text)),
                Arg::Count(count) => fluent.set(*name, FluentValue::from(*count as f64)),
            }
        }
        format(self.0, id, &fluent)
            .or_else(|| format(Language::En, id, &fluent))
            .unwrap_or_else(|| id.to_owned())
    }
}

fn format(language: Language, id: &str, args: &FluentArgs<'_>) -> Option<String> {
    let bundle = language.bundle();
    let pattern = bundle.get_message(id)?.value()?;
    let mut errors = Vec::new();
    let text = bundle.format_pattern(pattern, Some(args), &mut errors);
    errors.is_empty().then(|| text.into_owned())
}

/// `tr!(texts, "id", name = value, …)`: the text with arguments.
macro_rules! tr {
    ($texts:expr, $id:expr $(, $name:ident = $value:expr)* $(,)?) => {
        $texts.text($id, &[$((stringify!($name), $crate::i18n::Arg::from($value))),*])
    };
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use fluent_bundle::FluentResource;

    use super::*;

    fn ids(language: Language) -> BTreeSet<String> {
        let resource = FluentResource::try_new(language.source().to_owned())
            .unwrap_or_else(|(_, errors)| panic!("{}: {errors:?}", language.tag()));
        resource
            .entries()
            .filter_map(|entry| match entry {
                fluent_syntax::ast::Entry::Message(message) => Some(message.id.name.to_owned()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn every_language_has_the_ids_of_english() {
        let english = ids(Language::En);
        assert!(!english.is_empty());
        for &language in Language::ALL {
            let ids = ids(language);
            let missing: Vec<_> = english.difference(&ids).collect();
            let extra: Vec<_> = ids.difference(&english).collect();
            assert!(missing.is_empty(), "{} lacks {missing:?}", language.tag());
            assert!(extra.is_empty(), "{} has extra {extra:?}", language.tag());
        }
    }

    #[test]
    fn every_language_directory_is_listed() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../locales");
        let mut found: Vec<String> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        found.sort();
        let mut listed: Vec<_> = Language::ALL.iter().map(|l| l.tag().to_owned()).collect();
        listed.sort();
        assert_eq!(found, listed, "locales/ and Language::ALL differ");
    }

    #[test]
    fn tags_round_trip() {
        for &language in Language::ALL {
            assert_eq!(Language::from_tag(language.tag()), Some(language));
        }
        assert_eq!(Language::from_tag("de-AT"), Some(Language::De));
        assert_eq!(Language::from_tag("fr"), None);
    }

    #[test]
    fn locale_values_choose_the_language() {
        let pick = Language::from_locale;
        assert_eq!(pick(None, None, Some("de_DE.UTF-8")), Language::De);
        assert_eq!(pick(None, None, Some("en_US.UTF-8")), Language::En);
        assert_eq!(pick(None, None, Some("C")), Language::En);
        assert_eq!(pick(None, None, Some("POSIX")), Language::En);
        assert_eq!(pick(None, None, None), Language::En);
        assert_eq!(pick(None, None, Some("fr_FR")), Language::En);
        // The order is LC_ALL, LC_MESSAGES, LANG; empty values do not count.
        assert_eq!(pick(Some("de_DE"), None, Some("en_US")), Language::De);
        assert_eq!(pick(Some("C"), None, Some("de_DE")), Language::En);
        assert_eq!(pick(None, Some("de"), Some("en_US")), Language::De);
        assert_eq!(pick(Some(""), Some(""), Some("de_DE")), Language::De);
        assert_eq!(pick(Some("de_DE@euro"), None, None), Language::De);
    }

    #[test]
    fn raw_texts_keep_variables_and_cover_every_ui_id() {
        for &language in Language::ALL {
            let texts = language.raw_texts("ui-");
            let english = Language::En.raw_texts("ui-");
            assert_eq!(
                texts.keys().collect::<Vec<_>>(),
                english.keys().collect::<Vec<_>>()
            );
            assert!(texts.values().all(|text| !text.is_empty()));
        }
        let hint = Language::En.raw_texts("notice-compare-hint");
        assert!(hint["notice-compare-hint"].contains("check code"));
    }

    #[test]
    fn missing_ids_fall_back() {
        let de = Language::De.texts();
        assert_eq!(de.text("no-such-id", &[]), "no-such-id");
        assert_eq!(de.text("time-never", &[]), "nie");
        assert_eq!(Language::En.texts().text("time-never", &[]), "never");
    }
}
