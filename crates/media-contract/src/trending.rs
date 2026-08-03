use serde::{Deserialize, Serialize};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrendingCategoryDto {
    All,
    Movie,
    Tv,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrendingMediaTypeDto {
    Movie,
    Tv,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrendingItemDto {
    pub tmdb_id: u64,
    pub media_type: TrendingMediaTypeDto,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poster_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrendingPageDto {
    pub source: String,
    pub window: String,
    pub category: TrendingCategoryDto,
    pub page: u32,
    pub total_pages: u32,
    pub total_results: u32,
    pub results: Vec<TrendingItemDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaDetailsDto {
    pub tmdb_id: u64,
    pub media_type: TrendingMediaTypeDto,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poster_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overview: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub countries: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season_count: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_episode: Option<UpcomingEpisodeDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmdb_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trailer_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpcomingEpisodeDto {
    pub season: u16,
    pub episode: u16,
    pub air_date: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimilarPageDto {
    pub source: String,
    pub tmdb_id: u64,
    pub media_type: TrendingMediaTypeDto,
    pub page: u32,
    pub total_pages: u32,
    pub total_results: u32,
    pub results: Vec<TrendingItemDto>,
}
