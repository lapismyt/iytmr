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
    "mashup",
];

#[derive(Debug, PartialEq, Eq)]
pub struct TrackTitlePerformer {
    pub title: String,
    pub performer: String,
}

fn is_remix(query: &str) -> bool {
    let query = query.to_lowercase();

    REMIX_KEYWORDS.iter().any(|kw| query.contains(kw))
}

pub fn get_title_and_perfomer(
    video_title: &str,
    video_uploader: Option<&str>,
) -> TrackTitlePerformer {
    let video_uploader = video_uploader
        .map(|u| u.strip_suffix(" - Topic").unwrap_or(u))
        .unwrap_or("???");

    if is_remix(video_title) {
        return TrackTitlePerformer {
            title: video_title.to_string(),
            performer: video_uploader.to_string(),
        };
    }

    if video_title.contains(video_uploader) {
        for sep in PERFORMER_TITLE_SEPARATORS {
            let performer_prefix = format!("{}{}", video_uploader, sep);
            let performer_suffix = format!("{}{}", sep, video_uploader);

            if let Some(title_stripped) = video_title.strip_prefix(&performer_prefix) {
                return TrackTitlePerformer {
                    title: title_stripped.to_string(),
                    performer: video_uploader.to_string(),
                };
            }

            if let Some(title_stripped) = video_title.strip_suffix(&performer_suffix) {
                return TrackTitlePerformer {
                    title: title_stripped.to_string(),
                    performer: video_uploader.to_string(),
                };
            }
        }
    }

    for sep in PERFORMER_TITLE_SEPARATORS {
        if let Some((result_performer, result_title)) = video_title.split_once(sep) {
            let performer = if video_uploader.to_lowercase() == result_performer.to_lowercase() {
                video_uploader
            } else {
                result_performer
            };
            return TrackTitlePerformer {
                title: result_title.to_string(),
                performer: performer.to_string(),
            };
        }
    }

    TrackTitlePerformer {
        title: video_title.to_string(),
        performer: video_uploader.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use crate::parser::{TrackTitlePerformer, get_title_and_perfomer};

    #[test]
    fn test_get_title_and_perfomer_1() {
        assert_eq!(
            get_title_and_perfomer(
                "17. Petal Dance (DELTARUNE Chapter 5 Soundtrack) - Toby Fox",
                Some("Toby Fox")
            ),
            TrackTitlePerformer {
                title: "17. Petal Dance (DELTARUNE Chapter 5 Soundtrack)".to_string(),
                performer: "Toby Fox".to_string()
            }
        )
    }

    #[test]
    fn test_get_title_and_perfomer_2() {
        assert_eq!(
            get_title_and_perfomer("femtanyl - LOTTERY", Some("Femtanyl")),
            TrackTitlePerformer {
                title: "LOTTERY".to_string(),
                performer: "Femtanyl".to_string()
            }
        )
    }

    #[test]
    fn test_get_title_and_perfomer_3() {
        assert_eq!(
            get_title_and_perfomer("Song For Wemmbu | DEADLOCK", Some("AZALI")),
            TrackTitlePerformer {
                title: "Song For Wemmbu | DEADLOCK".to_string(),
                performer: "AZALI".to_string()
            }
        )
    }

    #[test]
    fn test_get_title_and_perfomer_4() {
        assert_eq!(
            get_title_and_perfomer(
                "Deltarune - Petal Dance [Metal Remix by NyxTheShield]",
                Some("NyxTheShield OFFICIAL")
            ),
            TrackTitlePerformer {
                title: "Deltarune - Petal Dance [Metal Remix by NyxTheShield]".to_string(),
                performer: "NyxTheShield OFFICIAL".to_string()
            }
        )
    }

    #[test]
    fn test_get_title_and_perfomer_5() {
        assert_eq!(
            get_title_and_perfomer("Bromeliad (floopy Remix)", Some("Minecraft - Topic")),
            TrackTitlePerformer {
                title: "Bromeliad (floopy Remix)".to_string(),
                performer: "Minecraft".to_string()
            }
        )
    }
}
