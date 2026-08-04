use super::*;

#[test]
fn advertised_agents_appears_once_in_help_and_discovery() {
    let advertised = vec![
        ("agents".into(), "manage named sub-agents".into()),
        ("config".into(), "Lua duplicate".into()),
    ];
    let commands = merge_commands(&advertised);
    assert_eq!(
        commands.iter().filter(|(name, _)| name == "agents").count(),
        1
    );
    assert_eq!(
        commands.iter().filter(|(name, _)| name == "config").count(),
        1
    );
    assert!(help(&advertised).contains("/agents"));
}
