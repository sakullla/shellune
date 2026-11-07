#!/usr/bin/env python3
"""Generate Shellune icons using the configured Codex provider (adapted from Rillight).

Requires Python 3.11+ and Pillow. Use --reuse-source for offline regeneration.
"""
import argparse
import base64
import json
import os
from pathlib import Path
import tomllib
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen
from io import BytesIO
from PIL import Image, ImageOps

ROOT = Path(__file__).resolve().parents[1]
ICONS = ROOT / "assets/icons"
PROMPT = """Use case: logo-brand
Asset type: square desktop app icon for Shellune, a local shell, SSH and SFTP terminal.
Primary request: one original elegant crescent moon emblem whose inner negative space subtly forms a terminal prompt chevron. Simple confident silhouette, readable at 16px.
Style: polished flat 2D software brand mark, crisp curves, restrained soft highlight.
Color palette: moon-blue #8fa0ff, pale lavender #b48cff and cool silver; dark ink #0b0c10 rounded-square tile.
Composition: centered emblem, generous padding, flat front view, 1024x1024 square. Rounded tile occupies 90% of canvas; perfectly flat white outside tile for background removal.
Constraints: no words, no letters, no scenery, no stars, no tiny details, no 3D mockup, no watermark."""

def fail(message):
    raise SystemExit(f"Error: {message}")

def load_provider(config_path: Path) -> tuple[str, str, str, str]:
    if not config_path.is_file():
        fail(f"Codex config not found: {config_path}")

    with config_path.open("rb") as config_file:
        config = tomllib.load(config_file)

    provider_name = config.get("model_provider")
    providers = config.get("model_providers", {})
    provider = providers.get(provider_name, {})
    if not provider_name or not isinstance(provider, dict):
        fail("The selected Codex model provider is missing from config.toml.")

    base_url = provider.get("base_url")
    bearer_token = provider.get("experimental_bearer_token")
    response_model = config.get("model")
    if not base_url:
        fail("The selected Codex provider has no base_url.")
    if not bearer_token:
        fail("The selected Codex provider has no bearer token.")
    if not response_model:
        fail("Codex config has no Responses model.")

    return (
        str(provider_name),
        str(base_url).rstrip("/"),
        str(bearer_token),
        str(response_model),
    )


def request_json(
    url: str,
    bearer_token: str,
    *,
    payload: dict[str, Any] | None = None,
    timeout: int = 300,
) -> dict[str, Any]:
    body = None
    headers = {
        "Accept": "application/json",
        "Authorization": f"Bearer {bearer_token}",
    }
    if payload is not None:
        body = json.dumps(payload).encode("utf-8")
        headers["Content-Type"] = "application/json"

    request = Request(url, data=body, headers=headers, method="POST" if body else "GET")
    try:
        with urlopen(request, timeout=timeout) as response:
            response_body = response.read()
    except HTTPError as error:
        detail = error.read().decode("utf-8", errors="replace")
        fail(f"Image provider returned HTTP {error.code}: {detail[:1000]}")
    except URLError as error:
        fail(f"Could not reach image provider: {error.reason}")

    try:
        decoded = json.loads(response_body)
    except json.JSONDecodeError as error:
        fail(f"Image provider returned invalid JSON: {error}")
    if not isinstance(decoded, dict):
        fail("Image provider returned an unexpected response shape.")
    return decoded


def fetch_responses_image_bytes(response: dict[str, Any]) -> bytes:
    output = response.get("output")
    if not isinstance(output, list):
        fail("Responses image provider returned no output items.")

    for item in output:
        if not isinstance(item, dict) or item.get("type") != "image_generation_call":
            continue
        result = item.get("result")
        if not isinstance(result, str) or not result:
            continue
        if result.startswith("data:") and "," in result:
            result = result.split(",", 1)[1]
        try:
            return base64.b64decode(result)
        except ValueError as error:
            fail(f"Responses image provider returned invalid base64 data: {error}")

    output_types = [
        item.get("type")
        for item in output
        if isinstance(item, dict)
    ]
    fail(
        "Responses provider returned no completed image_generation_call "
        f"(output types: {output_types})."
    )



def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=Path(os.environ.get("CODEX_HOME", str(Path.home() / ".codex"))) / "config.toml")
    parser.add_argument("--model", default="gpt-image-2.5")
    parser.add_argument("--source", type=Path, default=ICONS / "shellune-source.png")
    parser.add_argument("--reuse-source", action="store_true")
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args()
    targets = [ICONS / "shellune.png", ICONS / "shellune.ico", ICONS / "shellune.icns"]
    if not args.force and any(p.exists() for p in targets):
        fail("Icons exist; use --force to replace them.")
    if args.reuse_source:
        source = args.source.read_bytes()
    else:
        if args.source.exists() and not args.force:
            fail("Source exists; use --reuse-source or --force.")
        provider, url, token, model = load_provider(args.config)
        print(f"Generating Shellune icon with {provider} / {args.model}", flush=True)
        response = request_json(f"{url}/responses", token, payload={
            "model": model, "input": PROMPT,
            "tools": [{"type": "image_generation", "model": args.model,
                       "action": "generate", "quality": "high"}],
        })
        source = fetch_responses_image_bytes(response)
        args.source.parent.mkdir(parents=True, exist_ok=True)
        args.source.write_bytes(source)
    with Image.open(BytesIO(source)) as original:
        image = ImageOps.fit(ImageOps.exif_transpose(original).convert("RGBA"), (1024, 1024), method=Image.Resampling.LANCZOS)
    # Remove only edge-connected white backdrop; preserve the pale moon highlight.
    from PIL import ImageDraw
    mask = image.convert("RGB")
    ImageDraw.floodfill(mask, (0, 0), (255, 0, 255), thresh=55)
    pixels = image.load()
    for y in range(image.height):
        for x in range(image.width):
            if mask.getpixel((x, y)) == (255, 0, 255):
                pixels[x, y] = (0, 0, 0, 0)
    ICONS.mkdir(parents=True, exist_ok=True)
    image.save(targets[0], optimize=True)
    image.save(targets[1], sizes=[(s, s) for s in (16, 24, 32, 48, 64, 128, 256)])
    image.save(targets[2], format="ICNS")
    for target in targets:
        print(target)

if __name__ == "__main__":
    main()
