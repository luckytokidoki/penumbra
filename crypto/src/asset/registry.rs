use std::sync::Arc;

use once_cell::sync::Lazy;
use regex::{Regex, RegexSet};

use crate::asset::{denom, Denom, Unit};

/// A registry of known assets, providing metadata related to a denomination string.
///
/// The [`REGISTRY`] constant provides an instance of the registry.
pub struct Registry {
    /// Individual regexes for base denominations
    base_regexes: Vec<Regex>,

    /// Set of regexes that matches any base denomination.
    base_set: RegexSet,

    /// Individual regexes for the display denominations, grouped by their base denomination.
    display_regexes: Vec<Vec<Regex>>,

    /// Set of regexes that matches any display denomination.
    display_set: RegexSet,

    /// Mapping from indices of `display_set` to indices of `base_regexes`.
    ///
    /// This allows looking up the base denomination for each display denomination.
    display_to_base: Vec<usize>,

    /// List of constructors for asset metadata, indexed by base denomination.
    ///
    /// Each constructor maps the value of the `data` named capture from the
    /// base OR display regex to the asset metadata.
    //
    // If we wanted to load registry data from a file in the future (would
    // require working out how to write closures), we could use boxed closures
    // instead of a function.
    constructors: Vec<fn(&str) -> denom::Inner>,
}

impl Registry {
    /// Attempt to parse the provided `raw_denom` as a base denomination.
    ///
    /// If the denomination is a known base denomination, returns `Some` with
    /// the parsed base denomination and associated display units.
    ///
    /// If the denomination is a known display unit, returns `None`.
    ///
    /// If the denomination is unknown, returns `Some` with the parsed base
    /// denomination and default display denomination (base = display).
    pub fn parse_denom(&self, raw_denom: &str) -> Option<Denom> {
        // We hope that our regexes are disjoint (TODO: add code to test this)
        // so that there will only ever be one match from the RegexSet.

        if let Some(base_index) = self.base_set.matches(raw_denom).iter().next() {
            // We've matched a base denomination.

            // Rematch with the specific pattern to obtain captured denomination data.
            let data = self.base_regexes[base_index]
                .captures(raw_denom)
                .expect("already checked this regex matches")
                .name("data")
                .map(|m| m.as_str())
                .unwrap_or("");

            Some(Denom {
                inner: Arc::new(self.constructors[base_index](data)),
            })
        } else if self.display_set.matches(raw_denom).iter().next().is_some() {
            // 2. This denom isn't a base denom, it's a display denom
            None
        } else {
            // 3. Fallthrough: create default base denom
            Some(Denom {
                inner: Arc::new(denom::Inner::new(raw_denom.to_string(), Vec::new())),
            })
        }
    }

    /// Parses the provided `raw_unit`, determining whether it is a display unit
    /// for another denomination or a base denomination itself.
    ///
    /// If the denomination is a known display denomination, returns a display
    /// denomination associated with that display denomination's base
    /// denomination. Otherwise, returns a display denomination associated with
    /// the input parsed as a base denomination.
    pub fn parse_unit(&self, raw_unit: &str) -> Unit {
        if let Some(display_index) = self.display_set.matches(raw_unit).iter().next() {
            let base_index = self.display_to_base[display_index];
            // We need to determine which unit we matched
            for (unit_index, regex) in self.display_regexes[base_index].iter().enumerate() {
                if let Some(capture) = regex.captures(raw_unit) {
                    let data = capture.name("data").map(|m| m.as_str()).unwrap_or("");
                    return Unit {
                        inner: Arc::new(self.constructors[base_index](data)),
                        unit_index,
                    };
                }
            }
            unreachable!("we matched one of the display regexes");
        } else {
            self.parse_denom(raw_unit)
                .expect("parse_base only returns None on display denom input")
                .base_unit()
        }
    }
}

#[derive(Default)]
struct Builder {
    base_regexes: Vec<&'static str>,
    constructors: Vec<fn(&str) -> denom::Inner>,
    unit_regexes: Vec<Vec<&'static str>>,
}

impl Builder {
    /// Add an asset to the registry.
    ///
    /// - `base_regex`: matches the base denomination, with optional named capture `data`.
    /// - `unit_regexes`: match display units, with optional named capture `data`.
    /// - `constructor`: maps `data` captured by a base OR display regex to the asset metadata,
    ///    recorded as a `denom::Inner`.
    ///
    /// If the `data` capture is present in *any* base or display regex, it must
    /// match *exactly* the same pattern in all of them, as it is the input to
    /// the constructor.  Also, the `units` passed to `denom::Inner` must be in
    /// the same order as the `display_regexes`.
    fn add_asset(
        mut self,
        base_regex: &'static str,
        unit_regexes: &[&'static str],
        constructor: fn(&str) -> denom::Inner,
    ) -> Self {
        self.base_regexes.push(base_regex);
        self.constructors.push(constructor);
        self.unit_regexes.push(unit_regexes.to_vec());

        self
    }

    fn build(self) -> Registry {
        let mut display_to_base = Vec::new();
        let mut display_regexes = Vec::new();
        for (base_index, displays) in self.unit_regexes.iter().enumerate() {
            for _d in displays.iter() {
                display_to_base.push(base_index);
            }
            display_regexes.push(displays.iter().map(|d| Regex::new(d).unwrap()).collect());
        }

        Registry {
            base_set: RegexSet::new(self.base_regexes.iter()).unwrap(),
            base_regexes: self
                .base_regexes
                .iter()
                .map(|r| Regex::new(r).unwrap())
                .collect(),
            constructors: self.constructors,
            display_set: RegexSet::new(
                self.unit_regexes
                    .iter()
                    .flat_map(|displays| displays.iter()),
            )
            .unwrap(),
            display_to_base,
            display_regexes,
        }
    }
}

/// A fixed registry of known asset families.
pub static REGISTRY: Lazy<Registry> = Lazy::new(|| {
    Builder::default()
        .add_asset(
            "^upenumbra$",
            &["^penumbra$", "^mpenumbra$"],
            (|data: &str| {
                assert!(data.is_empty());
                denom::Inner::new(
                    "upenumbra".to_string(),
                    vec![
                        denom::UnitData {
                            exponent: 6,
                            denom: "penumbra".to_string(),
                        },
                        denom::UnitData {
                            exponent: 3,
                            denom: "mpenumbra".to_string(),
                        },
                    ],
                )
            }) as for<'r> fn(&'r str) -> _,
        )
        .add_asset(
            // Note: this regex must be in sync with DelegationToken::try_from
            // and VALIDATOR_IDENTITY_BECH32_PREFIX in the penumbra-stake crate
            // TODO: this doesn't restrict the length of the bech32 encoding
            "^udelegation_(?P<data>penumbravalid1[a-zA-HJ-NP-Z0-9]+)$",
            &[
                "^delegation_(?P<data>penumbravalid1[a-zA-HJ-NP-Z0-9]+)$",
                "^mdelegation_(?P<data>penumbravalid1[a-zA-HJ-NP-Z0-9]+)$",
            ],
            (|data: &str| {
                assert!(!data.is_empty());
                denom::Inner::new(
                    format!("udelegation_{}", data),
                    vec![
                        denom::UnitData {
                            exponent: 6,
                            denom: format!("delegation_{}", data),
                        },
                        denom::UnitData {
                            exponent: 3,
                            denom: format!("mdelegation_{}", data),
                        },
                    ],
                )
            }) as for<'r> fn(&'r str) -> _,
        )
        .build()
});
