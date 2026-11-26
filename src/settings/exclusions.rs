//! Compiles user rules for opting windows out of tiling. Constraints within a
//! rule must all match, and any matching rule excludes the window. The compiled
//! matcher records which properties to query so ordinary window events can
//! avoid reading unused process names, titles or classes.

use crate::i18n::{Msg, render, tr};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

const MAX_RULES: usize = 128;
const MAX_FIELD_CHARS: usize = 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum MatchMode {
    #[default]
    Equals,
    Contains,
    Regex,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ExclusionRule {
    pub mode: MatchMode,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub process: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub class: String,
}

#[derive(Debug)]
struct CompiledRule {
    process: Option<Regex>,
    title: Option<Regex>,
    class: Option<Regex>,
}

impl CompiledRule {
    fn new(rule: &ExclusionRule) -> Result<Self, String> {
        let compile = |value: &str, field: &str| -> Result<Option<Regex>, String> {
            if value.chars().count() > MAX_FIELD_CHARS {
                return Err(render(
                    Msg::RuleTooLong,
                    &[("field", field), ("limit", &MAX_FIELD_CHARS.to_string())],
                ));
            }
            if value.trim().is_empty() {
                return Ok(None);
            }
            let pattern = match rule.mode {
                MatchMode::Equals => format!(r"\A{}\z", regex::escape(value)),
                MatchMode::Contains => regex::escape(value),
                MatchMode::Regex => value.to_owned(),
            };
            RegexBuilder::new(&pattern)
                .case_insensitive(true)
                .size_limit(512 * 1024)
                .dfa_size_limit(64 * 1024)
                .build()
                .map(Some)
                .map_err(|error| {
                    let details = error.to_string();
                    let reason = details.lines().last().unwrap_or(&details);
                    render(
                        Msg::RuleInvalid,
                        &[
                            ("field", field),
                            ("reason", reason.trim_start_matches("error: ")),
                        ],
                    )
                })
        };
        let compiled = Self {
            process: compile(&rule.process, tr(Msg::RuleProcess))?,
            title: compile(&rule.title, tr(Msg::RuleTitle))?,
            class: compile(&rule.class, tr(Msg::RuleClass))?,
        };
        if compiled.process.is_none() && compiled.title.is_none() && compiled.class.is_none() {
            return Err(tr(Msg::RuleRequired).into());
        }
        Ok(compiled)
    }

    fn matches(&self, process: &str, title: &str, class: &str) -> bool {
        // A missing title or a denied process query is not evidence of a match,
        // even if a regular expression such as .* would accept an empty string.
        let matches = |pattern: &Option<Regex>, value: &str| {
            pattern
                .as_ref()
                .is_none_or(|re| !value.is_empty() && re.is_match(value))
        };
        matches(&self.process, process)
            && matches(&self.class, class)
            && matches(&self.title, title)
    }
}

fn rule_limit() -> String {
    render(Msg::RuleLimit, &[("limit", &MAX_RULES.to_string())])
}

fn rule_error(index: usize, reason: &str) -> String {
    render(
        Msg::RuleAtIndex,
        &[("number", &(index + 1).to_string()), ("reason", reason)],
    )
}

#[derive(Debug, Default)]
pub(crate) struct ExclusionMatcher {
    rules: Vec<CompiledRule>,
    needs_process: bool,
    needs_title: bool,
    needs_class: bool,
}

impl ExclusionMatcher {
    fn add(&mut self, rule: &ExclusionRule) -> Result<(), String> {
        let compiled = CompiledRule::new(rule)?;
        self.needs_process |= compiled.process.is_some();
        self.needs_title |= compiled.title.is_some();
        self.needs_class |= compiled.class.is_some();
        self.rules.push(compiled);
        Ok(())
    }

    /// Editor changes are atomic. Every proposed rule must be usable.
    pub fn compile(rules: &[ExclusionRule]) -> Result<Self, String> {
        let (matcher, warning) = Self::from_saved(rules);
        match warning {
            Some(reason) => Err(reason),
            None => Ok(matcher),
        }
    }

    pub fn compile_update(
        previous: &[ExclusionRule],
        next: &[ExclusionRule],
    ) -> Result<Self, String> {
        if previous.is_empty() {
            return Self::compile(next);
        }
        if next.len() > MAX_RULES && next.len() >= previous.len() {
            return Err(rule_limit());
        }
        let mut matcher = Self::default();
        let mut old_counts = None;
        for (index, rule) in next.iter().enumerate() {
            let error = if index >= MAX_RULES {
                // A user can shrink an oversized hand-edited profile in steps.
                // Entries past the limit stay inactive until they fit.
                rule_limit()
            } else if let Err(reason) = matcher.add(rule) {
                rule_error(index, &reason)
            } else {
                continue;
            };
            // An existing broken row must be repairable even if others remain.
            // Only count old text on error; ordinary saves incur no lookup cost.
            let counts = old_counts.get_or_insert_with(|| {
                let mut counts = HashMap::new();
                for old in previous {
                    *counts.entry(old).or_insert(0usize) += 1;
                }
                counts
            });
            match counts.get_mut(rule) {
                Some(count) if *count > 0 => *count -= 1,
                _ => return Err(error),
            }
        }
        Ok(matcher)
    }

    /// A typo in a hand-edited profile should leave the other rules active
    /// and the original text available for correction in the editor.
    pub fn from_saved(rules: &[ExclusionRule]) -> (Self, Option<String>) {
        let mut matcher = Self::default();
        let mut warning = (rules.len() > MAX_RULES).then(rule_limit);
        for (index, rule) in rules.iter().take(MAX_RULES).enumerate() {
            if let Err(reason) = matcher.add(rule) {
                warning.get_or_insert_with(|| rule_error(index, &reason));
            }
        }
        (matcher, warning)
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn needs_process(&self) -> bool {
        self.needs_process
    }

    pub fn needs_title(&self) -> bool {
        self.needs_title
    }

    pub fn needs_class(&self) -> bool {
        self.needs_class
    }

    pub fn may_match_pending_title(&self, process: &str, class: &str) -> bool {
        // During CREATE, even process and class can still be unavailable.
        // Only an already known mismatch rules out a future title match.
        let possible = |pattern: &Option<Regex>, value: &str| {
            value.is_empty() || pattern.as_ref().is_none_or(|re| re.is_match(value))
        };
        self.rules.iter().any(|rule| {
            rule.title.is_some() && possible(&rule.process, process) && possible(&rule.class, class)
        })
    }

    pub fn matches(&self, process: &str, title: &str, class: &str) -> bool {
        self.rules
            .iter()
            .any(|rule| rule.matches(process, title, class))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[test]
    fn matching_combines_fields_and_keeps_alternative_rules_independent() {
        let rules = [
            ExclusionRule {
                process: "Browser.EXE".into(),
                title: "Видеозвонок [1]".into(),
                ..Default::default()
            },
            ExclusionRule {
                class: ".tooltip".into(),
                mode: MatchMode::Contains,
                ..Default::default()
            },
            ExclusionRule {
                title: r"^(picture|video)-in-picture$".into(),
                mode: MatchMode::Regex,
                ..Default::default()
            },
        ];
        let matcher = ExclusionMatcher::compile(&rules).unwrap();
        assert!(matcher.needs_title() && matcher.needs_class() && matcher.needs_process());
        assert!(matcher.matches("browser.exe", "ВИДЕОЗВОНОК [1]", ""));
        assert!(!matcher.matches("other.exe", "Видеозвонок [1]", ""));
        assert!(!matcher.matches("Browser.EXE", "Видеозвонок 1", ""));
        assert!(!matcher.matches("Browser.EXE", "Подсказка Видеозвонок [1]", ""));
        assert!(matcher.matches("", "", "UI.ToolTip.Popup"));
        assert!(!matcher.matches("", "", "UI-ToolTip.Popup"));
        assert!(matcher.matches("", "PICTURE-in-PICTURE", ""));
        assert!(!matcher.matches("", "Picture-in-picture tab", ""));
    }

    #[test]
    fn missing_metadata_cannot_satisfy_a_regex_condition() {
        let rules = [ExclusionRule {
            title: ".*".into(),
            mode: MatchMode::Regex,
            ..Default::default()
        }];
        let matcher = ExclusionMatcher::compile(&rules).unwrap();
        assert!(!matcher.needs_process() && !matcher.needs_class());
        assert!(!matcher.matches("some.exe", "", "SomeClass"));
        assert!(matcher.matches("", "A", ""));
    }

    #[test]
    fn only_a_plausible_title_rule_defers_a_window_without_a_title() {
        let matcher = ExclusionMatcher::compile(&[
            ExclusionRule {
                process: "other.exe".into(),
                ..Default::default()
            },
            ExclusionRule {
                process: "browser.exe".into(),
                class: "popup".into(),
                title: "Video".into(),
                ..Default::default()
            },
        ])
        .unwrap();
        assert!(matcher.may_match_pending_title("BROWSER.exe", "Popup"));
        assert!(matcher.may_match_pending_title("", "Popup"));
        assert!(matcher.may_match_pending_title("browser.exe", ""));
        assert!(!matcher.may_match_pending_title("other.exe", "popup"));
        assert!(!matcher.may_match_pending_title("browser.exe", "main"));
    }

    #[test]
    fn old_profiles_and_rules_round_trip_even_when_a_regex_needs_correction() {
        let old: Settings = toml::from_str("gap = 11").unwrap();
        assert!(old.exclusions.is_empty());
        assert!(!toml::to_string(&old).unwrap().contains("exclusions"));
        let raw = "gap = 11\n[[exclusions]]\nprocess = 'Good.exe'\n\
                   [[exclusions]]\nmode = 'regex'\ntitle = '['";
        let profile = toml::from_str::<Settings>(raw)
            .unwrap()
            .validated()
            .unwrap();
        let (matcher, warning) = ExclusionMatcher::from_saved(&profile.exclusions);
        assert!(matcher.matches("good.exe", "", ""));
        let invalid = CompiledRule::new(&profile.exclusions[1]).unwrap_err();
        assert_eq!(warning.unwrap(), rule_error(1, &invalid));
        assert!(ExclusionMatcher::compile(&profile.exclusions).is_err());
        let reloaded: Settings =
            toml::from_str(&toml::to_string_pretty(&profile).unwrap()).unwrap();
        assert_eq!(reloaded, profile);
        assert_eq!(profile.gap, 11);
        assert_eq!(reloaded.exclusions[1].title, "[");
    }

    #[test]
    fn validation_rejects_blank_and_overlong_rules_but_an_empty_list_means_no_exclusions() {
        assert!(ExclusionMatcher::compile(&[]).unwrap().is_empty());
        let mut rule = ExclusionRule {
            title: " \t".into(),
            ..Default::default()
        };
        assert!(ExclusionMatcher::compile(std::slice::from_ref(&rule)).is_err());
        rule.title = "x".repeat(MAX_FIELD_CHARS + 1);
        assert!(ExclusionMatcher::compile(std::slice::from_ref(&rule)).is_err());
        rule.title = "(".into(); // Literal modes accept regex metacharacters.
        assert!(ExclusionMatcher::compile(std::slice::from_ref(&rule)).is_ok());
        assert!(ExclusionMatcher::compile(&vec![rule; MAX_RULES + 1]).is_err());
    }

    #[test]
    fn a_bad_manual_rule_can_be_repaired_while_other_bad_rows_remain() {
        let first = ExclusionRule {
            title: "[".into(),
            mode: MatchMode::Regex,
            ..Default::default()
        };
        let second = ExclusionRule {
            title: "(".into(),
            mode: MatchMode::Regex,
            ..Default::default()
        };
        let before = [first.clone(), second.clone()];
        let repaired = ExclusionRule {
            title: "ok".into(),
            ..Default::default()
        };
        let after = [first.clone(), repaired.clone()];
        assert!(
            ExclusionMatcher::compile_update(&before, &after)
                .unwrap()
                .matches("", "ok", "")
        );
        assert!(
            ExclusionMatcher::compile_update(&before, &[first.clone(), first.clone()]).is_err()
        );
        assert!(ExclusionMatcher::compile_update(&after, &[repaired.clone(), second]).is_err());
        assert!(
            ExclusionMatcher::compile_update(&before, &[first])
                .unwrap()
                .is_empty()
        );
        let oversized = vec![repaired; MAX_RULES + 2];
        assert!(ExclusionMatcher::compile_update(&[], &oversized).is_err());
        let smaller = &oversized[..MAX_RULES + 1];
        assert!(ExclusionMatcher::compile_update(smaller, smaller).is_err());
        assert!(ExclusionMatcher::compile_update(&oversized, smaller).is_ok());
    }
}
