use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Image,
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceStatus {
    Online,
    Offline,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplingMode {
    FixedInterval,
    FrameInterval,
    TargetCount,
    ValidRanges,
    ChangeTriggered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoRange {
    pub start_ms: u64,
    pub end_ms: u64,
}

impl VideoRange {
    pub fn validate(self, duration_ms: u64) -> Result<Self, DomainError> {
        if self.start_ms >= self.end_ms {
            return Err(DomainError::InvalidVideoRange);
        }
        if self.end_ms > duration_ms {
            return Err(DomainError::VideoRangeOutOfBounds);
        }
        Ok(self)
    }
}

impl SourceKind {
    pub fn from_path(path: &std::path::Path) -> Option<Self> {
        let extension = path.extension()?.to_string_lossy().to_ascii_lowercase();
        match extension.as_str() {
            "jpg" | "jpeg" | "png" | "bmp" | "tif" | "tiff" | "webp" => Some(Self::Image),
            "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" | "mts" | "m2ts" => Some(Self::Video),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeStrategy {
    Discard,
    Pad,
    ShiftToEdge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Roi {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Roi {
    pub fn validate(self, source_width: u32, source_height: u32) -> Result<Self, DomainError> {
        if self.width == 0 || self.height == 0 {
            return Err(DomainError::EmptyRoi);
        }
        let right = self
            .x
            .checked_add(self.width)
            .ok_or(DomainError::RoiOutOfBounds)?;
        let bottom = self
            .y
            .checked_add(self.height)
            .ok_or(DomainError::RoiOutOfBounds)?;
        if right > source_width || bottom > source_height {
            return Err(DomainError::RoiOutOfBounds);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileConfig {
    pub tile_width: u32,
    pub tile_height: u32,
    pub overlap_x: u32,
    pub overlap_y: u32,
    pub edge_strategy: EdgeStrategy,
}

impl TileConfig {
    pub fn validate(self) -> Result<Self, DomainError> {
        if self.tile_width == 0 || self.tile_height == 0 {
            return Err(DomainError::EmptyTile);
        }
        if self.overlap_x >= self.tile_width || self.overlap_y >= self.tile_height {
            return Err(DomainError::OverlapTooLarge);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Draft,
    Estimated,
    Queued,
    Running,
    AwaitingReview,
    Exporting,
    Completed,
    CompletedWithErrors,
    Cancelling,
    Cancelled,
    Interrupted,
    Failed,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("ROI width and height must be greater than zero")]
    EmptyRoi,
    #[error("ROI is outside the source bounds")]
    RoiOutOfBounds,
    #[error("tile width and height must be greater than zero")]
    EmptyTile,
    #[error("tile overlap must be smaller than the tile dimensions")]
    OverlapTooLarge,
    #[error("video range start must be before end")]
    InvalidVideoRange,
    #[error("video range exceeds the source duration")]
    VideoRangeOutOfBounds,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_roi_outside_source() {
        let result = Roi {
            x: 90,
            y: 10,
            width: 20,
            height: 20,
        }
        .validate(100, 100);
        assert_eq!(result, Err(DomainError::RoiOutOfBounds));
    }

    #[test]
    fn rejects_overlap_equal_to_tile_size() {
        let result = TileConfig {
            tile_width: 64,
            tile_height: 64,
            overlap_x: 64,
            overlap_y: 0,
            edge_strategy: EdgeStrategy::Discard,
        }
        .validate();
        assert_eq!(result, Err(DomainError::OverlapTooLarge));
    }

    #[test]
    fn rejects_reversed_video_range() {
        assert_eq!(
            VideoRange {
                start_ms: 500,
                end_ms: 400,
            }
            .validate(1_000),
            Err(DomainError::InvalidVideoRange)
        );
    }
}
