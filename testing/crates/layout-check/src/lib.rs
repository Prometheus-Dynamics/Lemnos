//! Checks a driver's layout assertions against the layout its vendor header gives.
//!
//! A driver's `layout.rs` asserts sizes, alignments, field offsets and request numbers, as the
//! numbers the vendor header gave when they were recorded. This tool runs bindgen on the header and
//! compares bindgen's reading of each recorded number. Each variant's [`Variant::pairs`] says which
//! C expression proves which assertion: the expression is an enum constant in a wrapper that
//! includes the header, so bindgen evaluates it with clang's layout rules.
//!
//! Without a header the tool runs nothing: it says so per variant, and the driver's own assertions
//! still compile and run in the gate.

use std::collections::HashMap;

mod specs;

pub use specs::VARIANTS;

/// One variant of a driver's layout: the header it needs and the facts it proves.
pub struct Variant {
    /// The driver, as the directory under `drivers/` names it.
    pub driver: &'static str,
    /// The variant within the driver (`default`, `v510`, `fe`, ...).
    pub name: &'static str,
    /// The headers the variant includes, as the check names them.
    pub needs: &'static str,
    /// Where those headers come from: the vendor source and its rev.
    pub source: &'static str,
    /// Repository-relative files that hold the driver's layout assertions.
    pub layout: &'static [&'static str],
    /// Macros the check defined on the command line, for clang.
    pub defines: &'static [&'static str],
    /// The C text the check included, with the variant's `#ifdef` branch already chosen.
    pub preamble: &'static str,
    /// Each fact: the Rust text left of `==` in the assertion, and the C expression that gives its value.
    pub pairs: &'static [(&'static str, &'static str)],
    /// Facts of the old check that no constant expression can give, with the reason.
    pub unmapped: &'static [&'static str],
    /// The header revision the record was taken from, as far as the repository says (`unknown` when
    /// it does not). A header is compared only when its version is this one.
    pub record: &'static str,
}

impl Variant {
    /// The header this variant needs, as the message gives it.
    pub fn not_run_message(&self) -> String {
        format!(
            "{}/{}: not run: needs {} from {}",
            self.driver, self.name, self.needs, self.source
        )
    }
}

/// One assertion of a layout file: the text left of `==`, and the number right of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assert {
    pub lhs: String,
    pub value: i128,
}

/// Parses every `assert!(<lhs> == <number>)` in `text`, in order. Other assertions are skipped.
pub fn parse_asserts(text: &str) -> Vec<Assert> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find("assert!(") {
        rest = &rest[at + "assert!(".len()..];
        let Some(eq) = rest.find(" == ") else { break };
        // The statement ends at its `;`: a left side such as `size_of::<T>()` has parentheses of its own.
        let end = rest.find(';').unwrap_or(rest.len());
        if eq > end {
            continue;
        }
        let lhs = rest[..eq].trim().to_string();
        let after = &rest[eq + " == ".len()..];
        let number: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == 'x' || *c == '_')
            .collect();
        if let Some(value) = parse_number(&number) {
            out.push(Assert { lhs, value });
        }
        rest = after;
    }
    out
}

/// A decimal or `0x` hexadecimal number, with `_` separators allowed.
pub fn parse_number(text: &str) -> Option<i128> {
    let clean: String = text.chars().filter(|c| *c != '_').collect();
    let clean = clean.trim_end_matches(|c: char| {
        c == 'u' || c == 'i' || c == 's' || c == 'z' || c == 'l' || c == 'L' || c == 'U'
    });
    if let Some(hex) = clean
        .strip_prefix("0x")
        .or_else(|| clean.strip_prefix("0X"))
    {
        i128::from_str_radix(hex, 16).ok()
    } else {
        clean.parse::<i128>().ok()
    }
}

/// The assertion each of the variant's pairs proves: the nth pair with a given text is the nth
/// assertion with that text, in the order the layout files give. `Err` names the first pair with
/// no assertion to match.
pub fn match_pairs(variant: &Variant, asserts: &[Assert]) -> Result<Vec<Assert>, String> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let mut out = Vec::with_capacity(variant.pairs.len());
    for (lhs, _) in variant.pairs {
        let nth = seen.entry(lhs).or_insert(0);
        let found = asserts.iter().filter(|a| a.lhs == *lhs).nth(*nth);
        match found {
            Some(a) => out.push(a.clone()),
            None => {
                return Err(format!(
                    "no layout assertion for `{lhs}` (occurrence {})",
                    *nth + 1
                ));
            }
        }
        *nth += 1;
    }
    Ok(out)
}

/// The wrapper bindgen reads: the variant's C text, then one enum constant per pair. An enum constant
/// is used because bindgen evaluates `sizeof` and `offsetof` in an enum initialiser but not in a macro.
pub fn wrapper_text(variant: &Variant) -> String {
    let all: Vec<usize> = (0..variant.pairs.len()).collect();
    wrapper_for(variant, &all)
}

/// The wrapper with only the pairs at `indices`: each constant keeps its index in the variant.
pub fn wrapper_for(variant: &Variant, indices: &[usize]) -> String {
    let mut out = String::from(variant.preamble);
    out.push_str("\nenum {\n");
    for &index in indices {
        let (_, expr) = variant.pairs[index];
        out.push_str(&format!("    LF_{index} = ({expr}),\n"));
    }
    out.push_str("};\n");
    out
}

/// The values bindgen gave the `LF_<n>` constants, by their index.
pub fn parse_consts(bindgen_output: &str) -> HashMap<usize, i128> {
    let mut out = HashMap::new();
    for line in bindgen_output.lines() {
        let Some(rest) = line.trim().strip_prefix("pub const LF_") else {
            continue;
        };
        let Some((index, tail)) = rest.split_once(':') else {
            continue;
        };
        let Ok(index) = index.trim().parse::<usize>() else {
            continue;
        };
        let Some((_, value)) = tail.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_end_matches(';').trim();
        if let Some(number) = parse_number(value) {
            out.insert(index, number);
        }
    }
    out
}

/// One fact that disagrees with the header.
#[derive(Debug, PartialEq, Eq)]
pub struct Difference {
    pub lhs: String,
    pub layout: i128,
    pub header: i128,
}

/// Compares every pair's recorded value with the header's, by index.
pub fn compare(
    variant: &Variant,
    asserts: &[Assert],
    header: &HashMap<usize, i128>,
) -> Result<Vec<Difference>, String> {
    let matched = match_pairs(variant, asserts)?;
    let mut out = Vec::new();
    for (index, (lhs, _)) in variant.pairs.iter().enumerate() {
        let Some(&value) = header.get(&index) else {
            return Err(format!(
                "the header gave no value for `{lhs}` (pair {index})"
            ));
        };
        if matched[index].value != value {
            out.push(Difference {
                lhs: (*lhs).to_string(),
                layout: matched[index].value,
                header: value,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "const _: () = {\n    assert!(size_of::<Interface>() == 48);\n    assert!(offset_of!(Interface, vcid) == 4);\n    assert!(RESET == 0x4004_7601);\n    assert!(size_of::<Interface>() == 48);\n};\n";

    #[test]
    fn asserts_are_read_in_order_with_their_numbers() {
        let asserts = parse_asserts(SAMPLE);
        assert_eq!(asserts.len(), 4);
        assert_eq!(
            asserts[0],
            Assert {
                lhs: "size_of::<Interface>()".into(),
                value: 48
            }
        );
        assert_eq!(asserts[1].lhs, "offset_of!(Interface, vcid)");
        assert_eq!(
            asserts[2],
            Assert {
                lhs: "RESET".into(),
                value: 0x4004_7601
            }
        );
    }

    #[test]
    fn repeated_texts_match_by_occurrence() {
        let asserts = parse_asserts(SAMPLE);
        let variant = Variant {
            driver: "d",
            name: "v",
            needs: "h.h",
            source: "s",
            layout: &[],
            defines: &[],
            preamble: "",
            pairs: &[
                ("size_of::<Interface>()", "sizeof(struct i)"),
                ("size_of::<Interface>()", "sizeof(struct i)"),
            ],
            unmapped: &[],
            record: "r",
        };
        let matched = match_pairs(&variant, &asserts).unwrap();
        assert_eq!(matched.len(), 2);
        assert_eq!(matched[0].value, 48);
        let missing = Variant {
            pairs: &[("RESET", "R"), ("RESET", "R")],
            ..variant
        };
        assert!(
            match_pairs(&missing, &asserts)
                .unwrap_err()
                .contains("occurrence 2")
        );
    }

    #[test]
    fn the_wrapper_has_one_enum_constant_per_pair() {
        let variant = Variant {
            driver: "d",
            name: "v",
            needs: "h.h",
            source: "s",
            layout: &[],
            defines: &[],
            preamble: "#include <stddef.h>\n",
            pairs: &[("a", "sizeof(struct x)"), ("b", "offsetof(struct x, f)")],
            unmapped: &[],
            record: "r",
        };
        let text = wrapper_text(&variant);
        assert!(text.starts_with("#include <stddef.h>\n"));
        assert!(text.contains("LF_0 = (sizeof(struct x)),"));
        assert!(text.contains("LF_1 = (offsetof(struct x, f)),"));
    }

    #[test]
    fn bindgen_constants_are_read_by_index() {
        let out = "/* automatically generated */\npub const LF_0: _bindgen_ty_1 = 8;\npub const LF_1: _bindgen_ty_1 = 20;\npub type _bindgen_ty_1 = ::core::ffi::c_uint;\n";
        let consts = parse_consts(out);
        assert_eq!(consts.get(&0), Some(&8));
        assert_eq!(consts.get(&1), Some(&20));
        assert_eq!(consts.len(), 2);
    }

    #[test]
    fn a_difference_is_reported_and_agreement_is_not() {
        let asserts = parse_asserts(SAMPLE);
        let variant = Variant {
            driver: "d",
            name: "v",
            needs: "h.h",
            source: "s",
            layout: &[],
            defines: &[],
            preamble: "",
            pairs: &[("size_of::<Interface>()", "x"), ("RESET", "y")],
            unmapped: &[],
            record: "r",
        };
        let header: HashMap<usize, i128> = HashMap::from([(0, 48), (1, 0x4004_7602)]);
        let diffs = compare(&variant, &asserts, &header).unwrap();
        assert_eq!(
            diffs,
            vec![Difference {
                lhs: "RESET".into(),
                layout: 0x4004_7601,
                header: 0x4004_7602
            }]
        );
    }

    #[test]
    fn the_not_run_message_names_the_header_and_its_source() {
        let variant = Variant {
            driver: "pisp",
            name: "fe",
            needs: "pisp_fe_config.h",
            source: "Linux source drivers/media/platform/raspberrypi/rp1_cfe",
            layout: &[],
            defines: &[],
            preamble: "",
            pairs: &[],
            unmapped: &[],
            record: "r",
        };
        assert_eq!(
            variant.not_run_message(),
            "pisp/fe: not run: needs pisp_fe_config.h from Linux source drivers/media/platform/raspberrypi/rp1_cfe"
        );
    }
}
