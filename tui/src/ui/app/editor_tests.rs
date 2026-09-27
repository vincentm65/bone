use super::*;

#[test]
fn editor_temp_paths_are_unique_and_cleaned_up() {
    let first = editor_temp_path().unwrap();
    let first_path = first.to_path_buf();
    let second = editor_temp_path().unwrap();
    let second_path = second.to_path_buf();

    assert_ne!(first_path, second_path);
    assert!(first_path.exists());
    assert!(second_path.exists());

    drop(first);
    drop(second);
    assert!(!first_path.exists());
    assert!(!second_path.exists());
}
