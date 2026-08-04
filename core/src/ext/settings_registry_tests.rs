use super::*;

fn field(key: &str, field_type: SettingsFieldType, default: ExtensionValue) -> SettingsField {
    SettingsField {
        key: key.into(),
        label: key.into(),
        field_type,
        options: Vec::new(),
        default,
        value: None,
        integer: None,
        min: None,
        max: None,
    }
}

fn page(namespace: &str, fields: Vec<SettingsField>) -> SettingsPage {
    SettingsPage {
        namespace: namespace.into(),
        title: namespace.into(),
        owner: "test.lua".into(),
        command: None,
        fields,
    }
}

#[test]
fn paths_and_names_are_strict() {
    assert_eq!(split_path("compact.auto").unwrap(), ("compact", "auto"));
    for path in ["compact", ".auto", "compact.", "a.b.c", "bad/name.key"] {
        assert!(split_path(path).is_err(), "accepted {path:?}");
    }
}

#[test]
fn registration_rejects_collisions_and_bad_schemas() {
    let mut registry = SettingsRegistry::default();
    registry
        .register(page(
            "compact",
            vec![field(
                "auto",
                SettingsFieldType::Bool,
                ExtensionValue::Bool(true),
            )],
        ))
        .unwrap();
    assert!(
        registry
            .register(page(
                "compact",
                vec![field(
                    "other",
                    SettingsFieldType::String,
                    ExtensionValue::String(String::new()),
                )],
            ))
            .is_err()
    );
    for namespace in ["general", "ui", "theme", "keymaps"] {
        assert!(
            registry
                .register(page(
                    namespace,
                    vec![field(
                        "other",
                        SettingsFieldType::String,
                        ExtensionValue::String(String::new()),
                    )],
                ))
                .is_err(),
            "accepted reserved namespace {namespace}"
        );
    }
    assert!(registry.register(page("empty", Vec::new())).is_err());
    assert!(
        registry
            .register(page(
                "bad",
                vec![
                    field("same", SettingsFieldType::Bool, ExtensionValue::Bool(true)),
                    field("same", SettingsFieldType::Bool, ExtensionValue::Bool(false)),
                ],
            ))
            .is_err()
    );
}

#[test]
fn number_and_enum_constraints_are_enforced() {
    let mut number = field(
        "limit",
        SettingsFieldType::Number,
        ExtensionValue::Number(10.0),
    );
    number.integer = Some(true);
    number.min = Some(1.0);
    number.max = Some(100.0);
    assert!(number.validate(&ExtensionValue::Number(1.0)).is_ok());
    assert!(number.validate(&ExtensionValue::Number(1.5)).is_err());
    assert!(number.validate(&ExtensionValue::Number(101.0)).is_err());

    let mut choice = field(
        "mode",
        SettingsFieldType::Enum,
        ExtensionValue::String("fast".into()),
    );
    choice.options = vec!["fast".into(), "safe".into()];
    assert!(
        choice
            .validate(&ExtensionValue::String("safe".into()))
            .is_ok()
    );
    assert!(
        choice
            .validate(&ExtensionValue::String("other".into()))
            .is_err()
    );
}
