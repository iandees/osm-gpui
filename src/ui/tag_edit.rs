//! Shared vocabulary for editing a single OSM tag in the side panel's Tags
//! table.

/// Which cell of a tag row an inline edit targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagEditField {
    /// The key cell of an existing row (a rename).
    Key,
    /// The value cell of an existing row.
    Value,
    /// Neither: the "add tag" row, where both cells are editable.
    None,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_variants_are_distinct() {
        assert_ne!(TagEditField::Key, TagEditField::Value);
        assert_ne!(TagEditField::Value, TagEditField::None);
    }
}
