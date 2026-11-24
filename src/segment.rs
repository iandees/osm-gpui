//! Click-to-object segmentation with MobileSAM (an ONNX image encoder plus a
//! mask decoder), run on the CPU through ONNX Runtime. The two model files are
//! a separate download (`models`), not part of the app.
//!
//! The encoder is the slow part (a fraction of a second) and is run once per
//! picture; the decoder turns the encoder's embedding and a click into a mask
//! in a few milliseconds, so the pointer can be followed live.

use std::path::{Path, PathBuf};

use ort::session::Session;
use ort::value::Tensor;

use crate::detect::Raster;

/// The encoder and decoder files, and where they come from.
pub mod models {
    use super::*;

    pub const ENCODER: &str = "mobile_sam_image_encoder.onnx";
    pub const DECODER: &str = "sam_mask_decoder_multi.onnx";
    const BASE_URL: &str = "https://huggingface.co/Acly/MobileSAM/resolve/main/";

    /// Where the model files live: next to the tile cache.
    pub fn dir() -> PathBuf {
        dirs::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("osm-gpui")
            .join("models")
            .join("mobile-sam")
    }

    /// Whether both files are present.
    pub fn present(dir: &Path) -> bool {
        [ENCODER, DECODER].iter().all(|f| dir.join(f).is_file())
    }

    /// The URL and file name of each model file.
    pub fn files() -> [(String, &'static str); 2] {
        [ENCODER, DECODER].map(|f| (format!("{BASE_URL}{f}"), f))
    }

    /// Download whichever model files are missing into `dir`, calling
    /// `progress(bytes_done, bytes_total)` across both. Each file is written
    /// under a temporary name and renamed once complete, so a half-finished
    /// download is never mistaken for a model. `keep` returning false
    /// abandons the download.
    pub fn download(
        client: &dyn crate::http::HttpClient,
        dir: &Path,
        progress: &mut dyn FnMut(u64, u64),
        keep: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        // Sizes are known roughly in advance, so progress can span both files.
        let expected: [u64; 2] = [28_157_093, 16_496_559];
        let total: u64 = expected.iter().sum();
        let mut done_before = 0u64;
        for ((url, name), size) in files().into_iter().zip(expected) {
            let target = dir.join(name);
            if target.is_file() {
                done_before += size;
                continue;
            }
            let req = crate::http::HttpRequest::get(&url);
            let resp = client
                .request_with_progress(req, &mut |so_far, _| {
                    progress(done_before + so_far, total);
                    keep()
                })
                .map_err(|e| format!("{e:?}"))?;
            let partial = dir.join(format!("{name}.part"));
            std::fs::write(&partial, &resp.body).map_err(|e| e.to_string())?;
            std::fs::rename(&partial, &target).map_err(|e| e.to_string())?;
            done_before += size;
        }
        Ok(())
    }
}

/// The encoder's working size: the picture is scaled so its longer side is
/// this many pixels.
const SIDE: usize = 1024;

/// What the encoder made of one picture, kept so any number of clicks on it
/// are cheap.
pub struct Embedding {
    data: Vec<f32>,
    /// The picture's size in its own pixels.
    pub width: usize,
    pub height: usize,
}

pub struct Segmenter {
    encoder: Session,
    decoder: Session,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Segmenter {
    /// Load the two model files from `dir`.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let environment = ort::environment::init()
            .with_name("osm-gpui")
            .build()
            .map_err(err)?;
        let open = |name: &str| -> Result<Session, String> {
            Session::builder(&environment)
                .map_err(err)?
                .commit_from_file(dir.join(name))
                .map_err(err)
        };
        Ok(Self {
            encoder: open(models::ENCODER)?,
            decoder: open(models::DECODER)?,
        })
    }

    /// Run the encoder over `picture` (unknown pixels count as black).
    pub fn embed(&mut self, picture: &Raster) -> Result<Embedding, String> {
        let (w, h) = (picture.width, picture.height);
        let mut img = image::RgbImage::new(w as u32, h as u32);
        for y in 0..h {
            for x in 0..w {
                let p = picture.at(x, y).unwrap_or([0, 0, 0]);
                img.put_pixel(x as u32, y as u32, image::Rgb(p));
            }
        }
        let scale = SIDE as f64 / w.max(h) as f64;
        let (sw, sh) = (
            ((w as f64 * scale).round() as u32).max(1),
            ((h as f64 * scale).round() as u32).max(1),
        );
        let scaled = image::imageops::resize(&img, sw, sh, image::imageops::FilterType::Triangle);
        let data: Vec<f32> = scaled.as_raw().iter().map(|&v| f32::from(v)).collect();
        let input = Tensor::from_array(([sh as usize, sw as usize, 3usize], data)).map_err(err)?;
        let outputs = self
            .encoder
            .run(ort::inputs!["input_image" => input])
            .map_err(err)?;
        let (_, embedding) = outputs["image_embeddings"]
            .try_extract_tensor::<f32>()
            .map_err(err)?;
        Ok(Embedding {
            data: embedding.to_vec(),
            width: w,
            height: h,
        })
    }

    /// The decoder's candidate masks for the object at `point` (picture
    /// pixels): `candidates_for` with that one point.
    pub fn candidates_at(
        &mut self,
        e: &Embedding,
        point: (f64, f64),
    ) -> Result<Vec<Candidate>, String> {
        self.candidates_for(e, &[point], &[])
    }

    /// The decoder's candidate masks for the object that contains every point
    /// of `include` and none of `exclude` (picture pixels), each with the
    /// model's confidence in it. The candidates run from a part of the object
    /// up to the whole of it; each is `width * height` flags, row by row. At
    /// least one point must be included.
    pub fn candidates_for(
        &mut self,
        e: &Embedding,
        include: &[(f64, f64)],
        exclude: &[(f64, f64)],
    ) -> Result<Vec<Candidate>, String> {
        if include.is_empty() {
            return Err("no point to segment around".to_string());
        }
        let scale = SIDE as f64 / e.width.max(e.height) as f64;
        let embeddings =
            Tensor::from_array(([1usize, 256, 64, 64], e.data.clone())).map_err(err)?;
        // Included points are labelled 1 and excluded ones 0; a last point
        // labelled -1 is the padding the decoder expects when there is no box.
        let mut coordinates: Vec<f32> = Vec::new();
        let mut labelling: Vec<f32> = Vec::new();
        for (points, label) in [(include, 1.0f32), (exclude, 0.0)] {
            for p in points {
                coordinates.extend([(p.0 * scale) as f32, (p.1 * scale) as f32]);
                labelling.push(label);
            }
        }
        coordinates.extend([0.0, 0.0]);
        labelling.push(-1.0);
        let count = labelling.len();
        let coords = Tensor::from_array(([1usize, count, 2], coordinates)).map_err(err)?;
        let labels = Tensor::from_array(([1usize, count], labelling)).map_err(err)?;
        let prior =
            Tensor::from_array(([1usize, 1, 256, 256], vec![0.0f32; 256 * 256])).map_err(err)?;
        let has_prior = Tensor::from_array(([1usize], vec![0.0f32])).map_err(err)?;
        let size =
            Tensor::from_array(([2usize], vec![e.height as f32, e.width as f32])).map_err(err)?;
        let outputs = self
            .decoder
            .run(ort::inputs![
                "image_embeddings" => embeddings,
                "point_coords" => coords,
                "point_labels" => labels,
                "mask_input" => prior,
                "has_mask_input" => has_prior,
                "orig_im_size" => size,
            ])
            .map_err(err)?;
        let (_, scores) = outputs["iou_predictions"]
            .try_extract_tensor::<f32>()
            .map_err(err)?;
        let (shape, masks) = outputs["masks"].try_extract_tensor::<f32>().map_err(err)?;
        let area = e.width * e.height;
        let kept = (shape[1] as usize).min(scores.len());
        Ok((0..kept)
            .map(|k| Candidate {
                confidence: scores[k],
                mask: masks[k * area..(k + 1) * area]
                    .iter()
                    .map(|&v| v > 0.0)
                    .collect(),
            })
            .collect())
    }
}

/// One mask the decoder proposes.
pub struct Candidate {
    /// The model's own estimate of how good the mask is (about 0 to 1).
    pub confidence: f32,
    pub mask: Vec<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs the model files: set `MOBILE_SAM_DIR` to run it.
    #[test]
    #[ignore]
    fn a_click_on_a_bright_square_masks_the_square() {
        let dir = std::env::var("MOBILE_SAM_DIR").expect("MOBILE_SAM_DIR");
        let mut seg = Segmenter::load(Path::new(&dir)).unwrap();
        let mut pic = Raster::new(256, 256);
        for y in 0..256 {
            for x in 0..256 {
                let inside = (80..180).contains(&x) && (70..170).contains(&y);
                pic.set(
                    x,
                    y,
                    if inside {
                        [230, 230, 230]
                    } else {
                        [40, 90, 40]
                    },
                );
            }
        }
        let e = seg.embed(&pic).unwrap();
        let candidates = seg.candidates_at(&e, (130.0, 120.0)).unwrap();
        let mask = &candidates
            .iter()
            .max_by(|a, b| a.confidence.total_cmp(&b.confidence))
            .unwrap()
            .mask;
        assert!(mask[120 * 256 + 130]);
        assert!(!mask[10 * 256 + 10]);
        let size = mask.iter().filter(|&&m| m).count();
        assert!((8000..12000).contains(&size), "{size}");
    }
}
