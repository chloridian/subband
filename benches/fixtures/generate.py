import random
import shutil
import subprocess
import tempfile
from pathlib import Path

from PIL import Image, ImageFilter


def pattern(mode, size):
    channels = len(mode)
    row = bytes((x * 31 + c * 63) % 256 for x in range(size) for c in range(channels))
    data = b"".join(bytes((v + y * 17) % 256 for v in row) for y in range(size))
    return Image.frombytes(mode, (size, size), data)


def detailed(width, height):
    rng = random.Random(1)
    gradient = Image.linear_gradient("L").resize((width, height))
    bands = []
    for angle in (0, 90, 180):
        noise = Image.frombytes("L", (width, height), rng.randbytes(width * height)).filter(ImageFilter.GaussianBlur(1.5))
        bands.append(Image.blend(gradient.rotate(angle), noise, 0.6))
    bands.append(gradient.rotate(270).point(lambda v: 255 if v > 96 else v * 2))
    return Image.merge("RGBA", bands)


def sculpt(size):
    return Image.merge("RGB", [Image.linear_gradient("L").resize((size, size)).rotate(angle) for angle in (0, 90, 180)])


def save(image, path, **options):
    image.save(path, format="JPEG2000", no_jp2=True, mct=1, quality_mode="rates", **options)
    print(path.name, path.stat().st_size)


def compress(image, path, *options):
    with tempfile.TemporaryDirectory() as scratch:
        source = Path(scratch) / "source.tga"
        image.save(source)
        subprocess.run([shutil.which("opj_compress"), "-i", source, "-o", path, *options], check=True,
                       stdout=subprocess.DEVNULL)
    print(path.name, path.stat().st_size)


if __name__ == "__main__":
    root = Path(__file__).parent
    save(pattern("LA", 8), root / "gray-alpha.j2c", num_resolutions=4, quality_layers=[1])
    save(Image.new("L", (4096, 4096), 73), root / "gray-4096.j2c", num_resolutions=6)
    save(detailed(2048, 2048), root / "rgba-2048-lossy.j2c", irreversible=True, num_resolutions=6,
         progression="RPCL", quality_layers=[128, 64, 32])
    compress(detailed(1024, 1024), root / "rgba-1024-lrcp.j2c", "-I", "-p", "LRCP", "-PLT", "-n", "6", "-b", "64,64",
             "-r", "6991,2097,524,131,8")
    compress(detailed(1024, 1024).convert("RGB"), root / "rgb-1024-rlcp.j2c", "-I", "-p", "RLCP", "-n", "6",
             "-r", "2048,512,128,32,8")
    compress(detailed(1024, 512), root / "rgba-1024x512-rlcp.j2c", "-I", "-p", "RLCP", "-n", "6",
             "-r", "2048,512,128,32,8")
    compress(sculpt(64), root / "sculpt-64.j2c", "-p", "RLCP", "-n", "7")
