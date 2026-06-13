use super::{civil_from_days, iso_from_unix_secs};

#[test]
fn unix_epoch_formats_as_valid_iso_date() {
    assert_eq!(iso_from_unix_secs(0), "1970-01-01T00:00:00Z");
}

#[test]
fn current_era_date_formats_as_valid_iso_date() {
    assert_eq!(iso_from_unix_secs(1_780_745_545), "2026-06-06T11:32:25Z");
    assert_eq!(civil_from_days(20_610), (2026, 6, 6));
}
