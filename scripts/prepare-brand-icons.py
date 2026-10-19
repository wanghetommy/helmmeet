"""Generate application branding from the approved SVG masters, using locked Tauri CLI.

No network, image-model calls or global tools. Run explicitly after editing the masters.
"""
from pathlib import Path
import shutil
import subprocess
import tempfile
from xml.etree import ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
BRAND = ROOT / 'docs/design/brand'


def tauri_icon(source, output, size=None):
    command = ['sh', 'scripts/with-toolchain.sh', 'npm', 'run', 'tauri', '-w',
               '@helmmeet/desktop', '--', 'icon', str(source), '--output', str(output)]
    if size:
        command.extend(['--png', str(size)])
    subprocess.run(command, cwd=ROOT, check=True)


def main():
    master = BRAND / 'helmmeet-logo.svg'
    ET.parse(master)
    ET.parse(BRAND / 'helmmeet-app-icon.svg')
    icons = ROOT / 'apps/desktop/src-tauri/icons'
    assets = ROOT / 'apps/desktop/src/assets'
    public = ROOT / 'apps/desktop/public'
    for directory in (icons, assets, public):
        directory.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='helmmeet-brand-icons-') as temp:
        stage = Path(temp)
        tauri_icon(BRAND / 'helmmeet-app-icon.svg', stage / 'app')
        # Template silhouettes adapt to the menu bar's actual light/dark appearance.
        # Keep all three paths; crop only outer canvas padding for the 18pt menu slot.
        template = master.read_text().replace('width="256" height="256" viewBox="0 0 256 256"',
                                              'width="216" height="216" viewBox="20 16 216 216"')
        template = template.replace('#0A3281', '#000000').replace('蓝色', '单色')
        template_file = stage / 'tray-template.svg'
        template_file.write_text(template)
        tauri_icon(template_file, stage / 'tray', 44)
        for filename in ('icon.png', 'icon.icns', '32x32.png', '128x128.png', '128x128@2x.png'):
            shutil.copyfile(stage / 'app' / filename, icons / filename)
        shutil.copyfile(stage / 'tray' / '44x44.png', icons / 'tray-template.png')
    shutil.copyfile(master, assets / 'helmmeet-logo.svg')
    shutil.copyfile(master, public / 'favicon.svg')
    print('Updated sidebar logo, favicon, macOS ICNS/PNG and 44px template tray icon.')


if __name__ == '__main__':
    main()
