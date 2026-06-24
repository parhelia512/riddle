use crate::html;

const INDEX_HTML: &str = include_str!("../assets/index.html");
const STYLE_CSS: &[u8] = include_bytes!("../assets/style.css");
const APP_JS: &[u8] = include_bytes!("../assets/app.js");

pub struct Asset {
    pub content_type: &'static str,
    pub body: &'static [u8],
}

pub fn page_html(source: &str) -> String {
    INDEX_HTML.replace("{{SOURCE}}", &html::escape(source))
}

pub fn asset(path: &str) -> Option<Asset> {
    match path {
        "/assets/style.css" => Some(Asset {
            content_type: "text/css; charset=utf-8",
            body: STYLE_CSS,
        }),
        "/assets/app.js" => Some(Asset {
            content_type: "application/javascript; charset=utf-8",
            body: APP_JS,
        }),
        _ => None,
    }
}
