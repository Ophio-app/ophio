// SPDX-License-Identifier: Apache-2.0
use super::*;

#[test]
fn retained_native_identity_survives_reordering_but_not_identical_replacement() {
    // Native uses the same matching function with CFEqual on retained AX refs.
    // These retained objects deliberately have identical labels and bounds.
    let first = Arc::new(("Fixture", [10, 20, 80, 30]));
    let second = Arc::new(("Fixture", [10, 20, 80, 30]));
    let previous = vec![
        Retained {
            id: "actual-native-1".into(),
            object: first.clone(),
        },
        Retained {
            id: "actual-native-2".into(),
            object: second.clone(),
        },
    ];
    let reordered = vec![
        Retained {
            id: "actual-native-2".into(),
            object: second.clone(),
        },
        Retained {
            id: "actual-native-1".into(),
            object: first.clone(),
        },
    ];
    assert_eq!(
        retained_id(&first, &reordered, &previous, Arc::ptr_eq).as_deref(),
        Some("actual-native-1")
    );
    assert_eq!(
        retained_id(&second, &reordered, &previous, Arc::ptr_eq).as_deref(),
        Some("actual-native-2")
    );
    let replacement = Arc::new(("Fixture", [10, 20, 80, 30]));
    assert!(retained_id(&replacement, &reordered, &previous, Arc::ptr_eq).is_none());
    // Keeping the old references retained prevents pointer reuse during matching.
    assert!(Arc::strong_count(&first) > 1);
    assert!(retained_id(&first, &[], &[], Arc::ptr_eq).is_none());
}
