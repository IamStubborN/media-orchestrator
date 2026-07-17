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
    pub original_title: Option<String>,
    pub year: Option<u16>,
    pub rating: Option<f32>,
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
