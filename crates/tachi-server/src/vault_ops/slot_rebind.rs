//! Lane-slot `vault set` is a rebind, not a silent ciphertext overwrite
//! (tachi#1855).
//!
//! EXTRACT/SUMMARY/DISTILL/REASONING slots used to `upsert` any new bytes
//! under the same name. Copying a GLM key into `EXTRACT_API_KEY` to escape a
//! SiliconFlow 402 left `vault list` looking unchanged. Compare keyed
//! fingerprints and require explicit `rebind` when the family changes.
//! Account names (`DEEPSEEK_API_KEY`, …) still rotate in place.

use memcore::vault::fingerprint::FingerprintKey;

/// Env names that are lane slots, not provider accounts.
pub(crate) const LANE_SLOT_SECRET_NAMES: &[&str] = &[
    "EXTRACT_API_KEY",
    "SUMMARY_API_KEY",
    "DISTILL_API_KEY",
    "REASONING_API_KEY",
];

pub(crate) fn is_lane_slot_secret_name(name: &str) -> bool {
    LANE_SLOT_SECRET_NAMES.contains(&name)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LaneSlotOverwrite {
    Identical { fingerprint: String },
    Rebound { old_fp: String, new_fp: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaneSlotRebindRequired {
    pub old_fp: String,
    pub new_fp: String,
}

impl LaneSlotRebindRequired {
    pub fn operator_message(&self, slot: &str) -> String {
        format!(
            "Lane slot '{slot}' is bound to fingerprint {}; new value is {}. \
             Pass rebind=true / --rebind to change account family. This is not a silent overwrite.",
            self.old_fp, self.new_fp
        )
    }
}

pub(crate) fn fingerprint_secret(
    master_key: &[u8; 32],
    provider_kind: &str,
    value: &str,
) -> String {
    FingerprintKey::derive_from_master_key(master_key).key_fingerprint(provider_kind, value)
}

pub(crate) fn evaluate_lane_slot_overwrite(
    old_value: &str,
    new_value: &str,
    provider_kind: &str,
    master_key: &[u8; 32],
    rebind: bool,
) -> Result<LaneSlotOverwrite, LaneSlotRebindRequired> {
    let old_fp = fingerprint_secret(master_key, provider_kind, old_value);
    let new_fp = fingerprint_secret(master_key, provider_kind, new_value);
    if old_fp == new_fp {
        return Ok(LaneSlotOverwrite::Identical {
            fingerprint: old_fp,
        });
    }
    if rebind {
        return Ok(LaneSlotOverwrite::Rebound { old_fp, new_fp });
    }
    Err(LaneSlotRebindRequired { old_fp, new_fp })
}

pub(crate) fn copy_existing_account_message(slot: &str, account_name: &str) -> String {
    format!(
        "Lane slot '{slot}' would copy ciphertext already stored as '{account_name}'. \
         Bind with {slot}=vault:{account_name} instead of storing a second copy."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER: [u8; 32] = [7u8; 32];

    #[test]
    fn lane_slot_names_are_the_four_chat_lanes() {
        assert!(is_lane_slot_secret_name("EXTRACT_API_KEY"));
        assert!(is_lane_slot_secret_name("DISTILL_API_KEY"));
        assert!(!is_lane_slot_secret_name("DEEPSEEK_API_KEY"));
        assert!(!is_lane_slot_secret_name("SILICONFLOW_API_KEY"));
        assert!(!is_lane_slot_secret_name("ZAI_API_KEY"));
    }

    #[test]
    fn identical_bytes_are_not_a_rebind() {
        let out =
            evaluate_lane_slot_overwrite("same-key", "same-key", "siliconflow", &MASTER, false)
                .expect("identical");
        match out {
            LaneSlotOverwrite::Identical { fingerprint } => {
                assert!(fingerprint.starts_with("fp1:"));
            }
            other => panic!("expected identical, got {other:?}"),
        }
    }

    #[test]
    fn different_bytes_without_rebind_are_refused() {
        let err = evaluate_lane_slot_overwrite(
            "siliconflow-key",
            "glm-key",
            "siliconflow",
            &MASTER,
            false,
        )
        .expect_err("must refuse");
        assert_ne!(err.old_fp, err.new_fp);
        let msg = err.operator_message("EXTRACT_API_KEY");
        assert!(msg.contains("EXTRACT_API_KEY"), "{msg}");
        assert!(msg.contains(&err.old_fp), "{msg}");
        assert!(msg.contains(&err.new_fp), "{msg}");
        assert!(
            msg.contains("--rebind") || msg.contains("rebind=true"),
            "{msg}"
        );
        assert!(!msg.contains("siliconflow-key"), "{msg}");
        assert!(!msg.contains("glm-key"), "{msg}");
    }

    #[test]
    fn different_bytes_with_rebind_are_allowed() {
        let out = evaluate_lane_slot_overwrite(
            "siliconflow-key",
            "glm-key",
            "siliconflow",
            &MASTER,
            true,
        )
        .expect("rebind");
        match out {
            LaneSlotOverwrite::Rebound { old_fp, new_fp } => {
                assert_ne!(old_fp, new_fp);
                assert!(old_fp.starts_with("fp1:"));
                assert!(new_fp.starts_with("fp1:"));
            }
            other => panic!("expected rebound, got {other:?}"),
        }
    }
}
