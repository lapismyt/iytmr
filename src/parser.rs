use regex::Regex;

const PERFORMER_TITLE_SEPARATORS: &[&str] = &[" — ", " - "];
const REMIX_KEYWORDS: &[&str] = &[
    "spedup",
    "remix",
    "edition",
    "sped up",
    "speedup",
    "speed up",
    "slowed",
    "reverb",
    "nightcore",
    "daycore",
    "cover",
    "hardstyle",
];

pub fn is_remix(query: &str) -> bool {
    let query = query.to_lowercase();

    REMIX_KEYWORDS.iter().any(|kw| query.contains(kw))
}

pub fn get_title_and_perfomer(video_title: &str, video_uploader: Option<&str>) -> (String, String) {
    let regex1 = Regex::new(r"\s*\(\d{4}\)\s*$").unwrap();
    let regex2 = Regex::new(r",\s*\d{4}\s*$").unwrap();

    let video_uploader = video_uploader.unwrap_or("???");

    let (result_title, result_performer) = match is_remix(video_title) {
        false => PERFORMER_TITLE_SEPARATORS
            .iter()
            .find_map(|sep| {
                if let Some((performer, title)) = video_title.split_once(sep) {
                    if !title.contains(video_uploader) {
                        return Some((title.trim(), performer.trim().to_owned()));
                    }
                }
                None
            })
            .unwrap_or((
                video_title,
                video_uploader
                    .strip_suffix(" - Topic")
                    .unwrap_or(video_uploader)
                    .to_owned(),
            )),
        true => (video_title, video_uploader.to_owned()),
    };

    let result_title = regex2
        .replace(&regex1.replace(&result_title, "").into_owned(), "")
        .into_owned();

    log::info!("title: {}, performer: {}", &result_title, &result_performer);

    (result_title, result_performer)
}
