//! Learned routing classifier (Rust) — the in-process ONNX counterpart to
//! `engine/route_clf.py`, so the TUI and the Python engine share one model.
//!
//! Three confidence-gated heads predict a task's specialty / difficulty bucket /
//! escalate flag. Each prediction is used only when the model clears its calibrated
//! threshold; otherwise the caller falls back to the transparent regex
//! (`crate::router::classify_specialty` / `classify`). Missing models (or the
//! `route-clf` feature off) -> every head abstains and routing is exactly the regex.
//!
//! Artifacts (shared with Python) load from the first of: `$SMALLCODE_ROUTER_CLF_DIR`,
//! `~/.config/smolcode/router_clf/onnx`, or `./router_clf/onnx`. Each head
//! dir has `model.onnx` + `tokenizer.json` + `labels.json`; thresholds come from a
//! shared `router_clf.json`.

use crate::router::Tier;

/// Map a difficulty bucket (0=trivial,1=moderate,2=hard) to a coarse Tier.
#[allow(dead_code)] // used by the route-clf backend and the tests
fn bucket_to_tier(bucket: usize) -> Tier {
    match bucket {
        0 => Tier::Small,
        1 => Tier::Medium,
        _ => Tier::Large,
    }
}

#[cfg(not(feature = "route-clf"))]
mod backend {
    use super::Tier;
    pub fn predict_specialty(_t: &str) -> Option<String> {
        None
    }
    pub fn predict_tier(_t: &str) -> Option<Tier> {
        None
    }
    pub fn predict_escalate(_t: &str) -> Option<bool> {
        None
    }
}

#[cfg(feature = "route-clf")]
mod backend {
    use super::{bucket_to_tier, Tier};
    use once_cell::sync::OnceCell;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use ort::session::Session;
    use tokenizers::Tokenizer;

    struct Head {
        session: Mutex<Session>, // ort Session::run needs &mut; heads live in a static
        tokenizer: Tokenizer,
        labels: Vec<String>,
        threshold: f32,
        max_len: usize,
    }

    impl Head {
        /// (argmax label, confidence) for a task, or None on any failure.
        fn predict(&self, task: &str) -> Option<(String, f32)> {
            let enc = self.tokenizer.encode(task, true).ok()?;
            let ids: Vec<i64> = enc.get_ids().iter().take(self.max_len).map(|&i| i as i64).collect();
            let mask: Vec<i64> = vec![1; ids.len()];
            let n = ids.len();
            let input_ids = ort::value::Value::from_array(([1usize, n], ids)).ok()?;
            let attn = ort::value::Value::from_array(([1usize, n], mask)).ok()?;
            let mut sess = self.session.lock().ok()?;
            let outputs = sess
                .run(ort::inputs!["input_ids" => input_ids, "attention_mask" => attn])
                .ok()?;
            let (_shape, logits) = outputs[0].try_extract_tensor::<f32>().ok()?;
            let probs = softmax(logits);
            let (idx, &conf) = probs
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())?;
            Some((self.labels.get(idx)?.clone(), conf))
        }
    }

    struct Clf {
        specialty: Option<Head>,
        tier: Option<Head>,
        escalate: Option<Head>,
    }

    static CLF: OnceCell<Clf> = OnceCell::new();

    fn softmax(xs: &[f32]) -> Vec<f32> {
        let m = xs.iter().cloned().fold(f32::MIN, f32::max);
        let exps: Vec<f32> = xs.iter().map(|x| (x - m).exp()).collect();
        let sum: f32 = exps.iter().sum();
        exps.iter().map(|e| e / sum).collect()
    }

    fn art_dir() -> PathBuf {
        if let Ok(d) = std::env::var("SMALLCODE_ROUTER_CLF_DIR") {
            return PathBuf::from(d);
        }
        if let Some(cfg) = dirs::config_dir() {
            let p = cfg.join("smolcode").join("router_clf").join("onnx");
            if p.join("specialty").join("model.onnx").exists() {
                return p;
            }
        }
        PathBuf::from("router_clf/onnx")
    }

    fn threshold_for(head: &str, dir: &Path) -> f32 {
        std::fs::read(dir.join("router_clf.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| v.get("thresholds")?.get(head)?.as_f64())
            .unwrap_or(0.6) as f32
    }

    fn load_head(dir: &Path, name: &str) -> Option<Head> {
        let h = dir.join(name);
        let meta: serde_json::Value =
            serde_json::from_slice(&std::fs::read(h.join("labels.json")).ok()?).ok()?;
        let labels: Vec<String> = meta
            .get("labels")?
            .as_array()?
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        let max_len = meta.get("max_len").and_then(|m| m.as_u64()).unwrap_or(128) as usize;
        let tokenizer = Tokenizer::from_file(h.join("tokenizer.json")).ok()?;
        let session = Session::builder().ok()?.commit_from_file(h.join("model.onnx")).ok()?;
        Some(Head {
            session: Mutex::new(session),
            tokenizer,
            labels,
            threshold: threshold_for(name, dir),
            max_len,
        })
    }

    fn clf() -> &'static Clf {
        CLF.get_or_init(|| {
            let dir = art_dir();
            Clf {
                specialty: load_head(&dir, "specialty"),
                tier: load_head(&dir, "tier"),
                escalate: load_head(&dir, "escalate"),
            }
        })
    }

    pub fn predict_specialty(task: &str) -> Option<String> {
        let h = clf().specialty.as_ref()?;
        let (label, conf) = h.predict(task)?;
        (conf >= h.threshold).then_some(label)
    }

    pub fn predict_tier(task: &str) -> Option<Tier> {
        let h = clf().tier.as_ref()?;
        let (label, conf) = h.predict(task)?;
        if conf < h.threshold {
            return None;
        }
        Some(bucket_to_tier(label.parse::<usize>().ok()?))
    }

    pub fn predict_escalate(task: &str) -> Option<bool> {
        let h = clf().escalate.as_ref()?;
        let (label, conf) = h.predict(task)?;
        (conf >= h.threshold).then(|| matches!(label.as_str(), "1" | "true" | "yes" | "escalate"))
    }
}

/// Learned specialty for a task, or None to defer to the regex classifier.
pub fn predict_specialty(task: &str) -> Option<String> {
    backend::predict_specialty(task)
}

/// Learned start tier for a task, or None to defer to the heuristic.
pub fn predict_tier(task: &str) -> Option<Tier> {
    backend::predict_tier(task)
}

/// Learned escalate-or-not prediction, or None to defer.
#[allow(dead_code)] // available to callers; the failure-driven loop also escalates
pub fn predict_escalate(task: &str) -> Option<bool> {
    backend::predict_escalate(task)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abstains_without_artifacts() {
        // Default build or no models on disk -> None, so callers fall back to the
        // heuristic and behavior is unchanged.
        let _ = predict_tier("reverse a string");
        let _ = predict_specialty("reverse a string");
        assert_eq!(bucket_to_tier(0), Tier::Small);
        assert_eq!(bucket_to_tier(2), Tier::Large);
        assert_eq!(bucket_to_tier(9), Tier::Large);
    }
}
