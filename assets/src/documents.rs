use serde::{Deserialize, Serialize};

pub(crate) const DEFAULT_DOCUMENT_ID: &str = "readme";
const DOCUMENTS_STORAGE_KEY: &str = "elixir-gpui.documents";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WorkspaceDocument {
    pub(crate) id: String,
    pub(crate) title: String,
}

pub(crate) fn default_documents() -> Vec<WorkspaceDocument> {
    vec![WorkspaceDocument {
        id: DEFAULT_DOCUMENT_ID.to_string(),
        title: "README.md".to_string(),
    }]
}

pub(crate) fn load_documents() -> Vec<WorkspaceDocument> {
    let Some(storage) = web_sys::window()
        .and_then(|window| window.local_storage().ok())
        .flatten()
    else {
        return default_documents();
    };
    let Some(value) = storage.get_item(DOCUMENTS_STORAGE_KEY).ok().flatten() else {
        return default_documents();
    };
    let documents = serde_json::from_str::<Vec<WorkspaceDocument>>(&value)
        .ok()
        .filter(|documents| !documents.is_empty())
        .unwrap_or_else(default_documents);
    normalize_documents(documents)
}

pub(crate) fn normalize_documents(mut documents: Vec<WorkspaceDocument>) -> Vec<WorkspaceDocument> {
    documents.retain(|document| document.id != "shared-notes");
    if !documents
        .iter()
        .any(|document| document.id == DEFAULT_DOCUMENT_ID)
    {
        documents.insert(0, default_documents().remove(0));
    }
    documents
}

pub(crate) fn save_documents(documents: &[WorkspaceDocument]) {
    let Some(storage) = web_sys::window()
        .and_then(|window| window.local_storage().ok())
        .flatten()
    else {
        return;
    };
    if let Ok(value) = serde_json::to_string(documents) {
        let _ = storage.set_item(DOCUMENTS_STORAGE_KEY, &value);
    }
}

pub(crate) fn document_title(title: &str) -> Option<String> {
    let title = title.trim();
    if title.is_empty() || title.len() > 253 {
        return None;
    }
    if title.to_lowercase().ends_with(".md") {
        Some(title.to_string())
    } else {
        Some(format!("{title}.md"))
    }
}

pub(crate) fn document_id_from_title(title: &str) -> String {
    let stem = title.strip_suffix(".md").unwrap_or(title);
    let mut id = String::new();
    let mut separator = false;
    for character in stem.chars() {
        if character.is_ascii_alphanumeric() {
            if separator && !id.is_empty() {
                id.push('-');
            }
            id.push(character.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
        if id.len() >= 48 {
            break;
        }
    }
    if !id.is_empty() {
        return id;
    }

    let hash = stem
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    format!("document-{hash:016x}")
}
