#[derive(Debug)]
pub struct TextEdit {
    pub offset: usize,
    pub delete_len: usize,
    pub insert: String,
}

impl TextEdit {
    pub fn summary(&self) -> String {
        format!(
            "@{} -{} +{}",
            self.offset,
            self.delete_len,
            self.insert.len()
        )
    }
}

pub fn single_edit(old: &str, new: &str) -> TextEdit {
    let mut prefix = 0;
    for ((old_i, old_ch), (new_i, new_ch)) in old.char_indices().zip(new.char_indices()) {
        if old_i != new_i || old_ch != new_ch {
            break;
        }
        prefix = old_i + old_ch.len_utf8();
    }

    let mut old_end = old.len();
    let mut new_end = new.len();
    while old_end > prefix && new_end > prefix {
        let Some((old_i, old_ch)) = old[..old_end].char_indices().next_back() else {
            break;
        };
        let Some((new_i, new_ch)) = new[..new_end].char_indices().next_back() else {
            break;
        };
        if old_i < prefix || new_i < prefix || old_ch != new_ch {
            break;
        }
        old_end = old_i;
        new_end = new_i;
    }

    TextEdit {
        offset: prefix,
        delete_len: old_end - prefix,
        insert: new[prefix..new_end].to_string(),
    }
}
