import argparse
import ctypes
import ctypes.wintypes
import json
import sys
from datetime import datetime
from pathlib import Path

try:
    from PIL import Image, ImageGrab
except ImportError as exc:
    raise SystemExit("Pillow is required: python -m pip install pillow") from exc


def foreground_window_rect():
    user32 = ctypes.windll.user32
    hwnd = user32.GetForegroundWindow()
    if not hwnd:
        raise RuntimeError("No foreground window is available to capture.")

    rect = ctypes.wintypes.RECT()
    if not user32.GetWindowRect(hwnd, ctypes.byref(rect)):
        raise RuntimeError("Failed to read the foreground window rectangle.")
    return (rect.left, rect.top, rect.right, rect.bottom)


def capture_active_window(output_dir):
    output_dir.mkdir(parents=True, exist_ok=True)
    path = output_dir / f"cli-layout-{datetime.now():%Y%m%d-%H%M%S}.png"
    image = ImageGrab.grab(bbox=foreground_window_rect())
    image.save(path)
    return path


def is_composer_pixel(pixel):
    red, green, blue = pixel[:3]
    spread = max(red, green, blue) - min(red, green, blue)
    return (
        32 <= red <= 62
        and 32 <= green <= 62
        and 32 <= blue <= 62
        and spread <= 8
    )


def is_footer_text_pixel(pixel):
    red, green, blue = pixel[:3]
    spread = max(red, green, blue) - min(red, green, blue)
    return (
        70 <= red <= 155
        and 70 <= green <= 155
        and 70 <= blue <= 155
        and spread <= 16
    )


def row_bands(rows, min_height):
    bands = []
    start = None
    for index, enabled in enumerate(rows):
        if enabled:
            if start is None:
                start = index
            continue
        if start is not None:
            height = index - start
            if height >= min_height:
                bands.append({"Start": start, "End": index - 1, "Height": height})
            start = None

    if start is not None:
        height = len(rows) - start
        if height >= min_height:
            bands.append({"Start": start, "End": len(rows) - 1, "Height": height})
    return bands


def analyze_image(path, args):
    image = Image.open(path).convert("RGB")
    width, height = image.size
    pixels = image.load()
    left = int(width * 0.01)
    right = int(width * 0.98)
    sample_step = max(1, (right - left) // 96)
    min_content_row = int(height * 0.12)

    composer_rows = []
    footer_text_rows = []
    for y in range(height):
        composer_hits = 0
        footer_hits = 0
        samples = 0
        for x in range(left, right, sample_step):
            pixel = pixels[x, y]
            if is_composer_pixel(pixel):
                composer_hits += 1
            if is_footer_text_pixel(pixel):
                footer_hits += 1
            samples += 1

        composer_rows.append(samples > 0 and composer_hits / samples >= 0.55)
        footer_text_rows.append(samples > 0 and footer_hits >= 2)

    composer_bands = [
        band
        for band in row_bands(composer_rows, args.min_band_height)
        if band["Start"] >= min_content_row
    ]
    composer_gaps = [
        {
            "FromBand": index - 1,
            "ToBand": index,
            "Gap": composer_bands[index]["Start"] - composer_bands[index - 1]["End"] - 1,
        }
        for index in range(1, len(composer_bands))
    ]
    last_composer_gap = composer_gaps[-1]["Gap"] if composer_gaps else None

    footer_start = None
    footer_gap = None
    if composer_bands:
        last_band_end = composer_bands[-1]["End"]
        for y in range(last_band_end, height):
            if footer_text_rows[y]:
                footer_start = y
                footer_gap = y - last_band_end - 1
                break

    passed = len(composer_bands) >= args.min_composer_bands
    if last_composer_gap is not None:
        passed = passed and last_composer_gap <= args.max_composer_gap_pixels
    if footer_gap is not None:
        passed = passed and footer_gap <= args.max_footer_gap_pixels

    return {
        "Passed": passed,
        "Image": str(Path(path).resolve()),
        "Width": width,
        "Height": height,
        "ComposerBandCount": len(composer_bands),
        "ComposerBands": composer_bands,
        "ComposerGaps": composer_gaps,
        "LastComposerGapPixels": last_composer_gap,
        "FooterStart": footer_start,
        "FooterGapPixels": footer_gap,
        "Thresholds": {
            "MinComposerBands": args.min_composer_bands,
            "MinBandHeight": args.min_band_height,
            "MaxComposerGapPixels": args.max_composer_gap_pixels,
            "MaxFooterGapPixels": args.max_footer_gap_pixels,
        },
    }


def parse_args():
    parser = argparse.ArgumentParser(
        description="Verify CodeForge CLI composer/footer layout from a screenshot."
    )
    parser.add_argument("--image", type=Path, help="PNG screenshot to analyze.")
    parser.add_argument(
        "--capture-active-window",
        action="store_true",
        help="Capture the current foreground window before analyzing.",
    )
    parser.add_argument("--min-composer-bands", type=int, default=3)
    parser.add_argument("--min-band-height", type=int, default=28)
    parser.add_argument("--max-composer-gap-pixels", type=int, default=28)
    parser.add_argument("--max-footer-gap-pixels", type=int, default=40)
    return parser.parse_args()


def main():
    args = parse_args()
    if not args.image and not args.capture_active_window:
        raise SystemExit("Pass --image <png> or --capture-active-window.")

    image_path = args.image
    if args.capture_active_window:
        image_path = capture_active_window(Path(".codeforge") / "layout-checks")

    result = analyze_image(image_path, args)
    print(json.dumps(result, indent=2))
    return 0 if result["Passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
