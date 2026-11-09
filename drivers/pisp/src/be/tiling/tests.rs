use super::*;

fn all(t: &Tiling) -> Vec<TileRegions> {
    (0..t.len()).map(|n| t.tile(n)).collect()
}

fn config(w: i32, h: i32, out: [Length2; 2]) -> TilingConfig {
    let full = Interval2 {
        x: Interval::new(0, w),
        y: Interval::new(0, h),
    };
    TilingConfig {
        input_image_size: Length2::new(w, h),
        crop: [full, full],
        output_image_size: out,
        max_tile_size: Length2::new(640, 3072),
        min_tile_size: Length2::new(16, 16),
        input_alignment: Length2::new(2, 2),
        output_max_alignment: [Length2::new(64, 2); 2],
        output_min_alignment: [Length2::new(16, 2); 2],
        ..Default::default()
    }
}

/// Every output pixel column is produced by exactly one tile, in order.
fn check_cover(tiles: &[TileRegions], grid: Length2, branch: usize, width: i32, height: i32) {
    let mut x = 0;
    for t in &tiles[..grid.dx as usize] {
        let o = t.output[branch].output.x;
        assert_eq!(o.offset, x, "gap or overlap at {x}");
        x = o.end();
    }
    assert_eq!(x, width);
    let mut y = 0;
    for row in 0..grid.dy as usize {
        let o = tiles[row * grid.dx as usize].output[branch].output.y;
        assert_eq!(o.offset, y);
        y = o.end();
    }
    assert_eq!(y, height);
    for t in tiles {
        assert!(t.input.input.x.length <= 640);
        assert!(t.input.input.x.length >= 16);
    }
}

#[test]
fn unscaled_1280x800_needs_three_columns() {
    let c = config(1280, 800, [Length2::new(1280, 800), Length2::default()]);
    let tiling = tile_pipeline(&c, 64).unwrap();
    let (tiles, grid) = (all(&tiling), tiling.grid());
    assert_eq!(grid, Length2::new(3, 1));
    check_cover(&tiles, grid, 0, 1280, 800);
    // Without resize blocks, what reaches the output is the input minus the crops.
    for t in &tiles {
        let crop = t.output[0].crop.x + t.crop[0].crop.x;
        let after = t.input.input.x.cropped(crop);
        assert_eq!(after.length, t.output[0].output.x.length);
    }
    // Interior tile edges carry 16 pixels of context.
    assert_eq!(
        (tiles[0].output[0].crop.x + tiles[0].crop[0].crop.x).end,
        16
    );
    assert_eq!(
        (tiles[1].output[0].crop.x + tiles[1].crop[0].crop.x).start,
        16
    );
}

#[test]
fn two_branches_with_resampling() {
    let mut c = config(1280, 800, [Length2::new(1280, 800), Length2::new(640, 400)]);
    c.resample_enables = 0b10;
    c.resample_factor[1] = Length2::new((1279 << 12) / 639, (799 << 12) / 399);
    let tiling = tile_pipeline(&c, 64).unwrap();
    let (tiles, grid) = (all(&tiling), tiling.grid());
    check_cover(&tiles, grid, 0, 1280, 800);
    check_cover(&tiles, grid, 1, 640, 400);
}

#[test]
fn downscale_then_resample() {
    let mut c = config(1920, 1080, [Length2::new(320, 180), Length2::default()]);
    c.downscale_enables = 0b01;
    c.resample_enables = 0b01;
    c.downscale_image_size[0] = Length2::new(640, 360);
    c.downscale_factor[0] = Length2::new((1920 << 12) / 640, (1080 << 12) / 360);
    c.resample_factor[0] = Length2::new((639 << 12) / 319, (359 << 12) / 179);
    let tiling = tile_pipeline(&c, 64).unwrap();
    let (tiles, grid) = (all(&tiling), tiling.grid());
    check_cover(&tiles, grid, 0, 320, 180);
}

#[test]
fn tall_images_get_rows_and_small_ones_one_tile() {
    let c = config(256, 4000, [Length2::new(256, 4000), Length2::default()]);
    let tiling = tile_pipeline(&c, 64).unwrap();
    let (tiles, grid) = (all(&tiling), tiling.grid());
    assert_eq!(grid.dx, 1);
    assert!(grid.dy >= 2);
    check_cover(&tiles, grid, 0, 256, 4000);
    let c = config(64, 64, [Length2::new(64, 64), Length2::default()]);
    let grid = tile_pipeline(&c, 64).unwrap().grid();
    assert_eq!(grid, Length2::new(1, 1));
}

#[test]
fn a_zero_alignment_is_an_error_not_a_division() {
    let mut c = config(64, 64, [Length2::new(64, 64), Length2::default()]);
    c.input_alignment = Length2::new(0, 2);
    assert_eq!(
        tile_pipeline(&c, 64).unwrap_err().0,
        "input alignment must be positive"
    );
    let mut c = config(64, 64, [Length2::new(64, 64), Length2::default()]);
    c.output_min_alignment[0] = Length2::new(0, 2);
    assert_eq!(
        tile_pipeline(&c, 64).unwrap_err().0,
        "output alignment must be positive"
    );
}

#[test]
fn a_zero_or_oversized_scale_factor_is_an_error() {
    let mut c = config(64, 64, [Length2::new(64, 64), Length2::default()]);
    c.resample_enables = 1;
    assert_eq!(
        tile_pipeline(&c, 64).unwrap_err().0,
        "resample factor must be positive"
    );
    let mut c = config(64, 64, [Length2::new(65535, 64), Length2::default()]);
    c.resample_enables = 1;
    c.resample_factor[0] = Length2::new(65535, 4096);
    assert_eq!(
        tile_pipeline(&c, 64).unwrap_err().0,
        "resample out of range"
    );
}

#[test]
fn the_tile_limit_is_capped_and_enforced() {
    let c = config(1280, 800, [Length2::new(1280, 800), Length2::default()]);
    assert_eq!(tile_pipeline(&c, 2).unwrap_err().0, "too many tiles");
    assert_eq!(tile_pipeline(&c, 0).unwrap_err().0, "too many tiles");
    assert_eq!(
        tile_pipeline(&c, usize::MAX).unwrap().grid(),
        Length2::new(3, 1)
    );
}

#[test]
fn no_outputs_is_an_error() {
    let c = config(64, 64, [Length2::default(); 2]);
    assert!(tile_pipeline(&c, 64).is_err());
}
