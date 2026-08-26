use crate::refs::RefEntry;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefVerification {
    pub matched: bool,
    pub missing: Vec<String>,
    pub mismatched: Vec<String>,
    pub excluded: Vec<String>,
}

pub fn verify_refs(source: &[RefEntry], target: &BTreeMap<String, String>) -> RefVerification {
    let mut missing = Vec::new();
    let mut mismatched = Vec::new();
    let mut excluded = Vec::new();
    for r in source {
        if r.decision != git_repo_migrator_domain::RefPolicyDecision::Allow {
            excluded.push(r.name.clone());
            continue;
        }
        match target.get(&r.name) {
            None => missing.push(r.name.clone()),
            Some(oid) if oid != &r.oid => mismatched.push(r.name.clone()),
            _ => {}
        }
    }
    RefVerification {
        matched: missing.is_empty() && mismatched.is_empty(),
        missing,
        mismatched,
        excluded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git_repo_migrator_domain::{RefClassification, RefPolicyDecision};
    #[test]
    fn verifies_tips() {
        let s = vec![RefEntry {
            name: "refs/heads/main".into(),
            oid: "abc".into(),
            classification: RefClassification::Branch,
            decision: RefPolicyDecision::Allow,
        }];
        let mut t = BTreeMap::new();
        t.insert("refs/heads/main".into(), "abc".into());
        assert!(verify_refs(&s, &t).matched);
    }
}
