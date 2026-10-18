"""Verify local Markdown links and required engineering handoff documents."""
import re
from pathlib import Path
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parents[1]
REQUIRED = ['README.md', 'AGENTS.md', 'docs/AI开发指南.md', 'docs/可行性评审.md',
            'docs/开发计划.md', 'docs/测试验收文档.md', 'docs/验证记录.md', 'docs/接口协议.md',
            'docs/本地会议助手_需求文档.md', 'docs/本地会议助手_技术方案.md']


def main():
    errors = [f'Missing: {name}' for name in REQUIRED if not (ROOT / name).is_file()]
    documents = [ROOT / 'README.md', ROOT / 'AGENTS.md', *sorted((ROOT / 'docs').rglob('*.md'))]
    for doc in documents:
        if not doc.exists():
            continue
        for target in re.findall(r'\[[^\]]+\]\(([^)]+)\)', doc.read_text()):
            target = target.strip('<>').split('#', 1)[0]
            if not target or re.match(r'^[a-zA-Z][a-zA-Z\d+.-]*:', target):
                continue
            if not (doc.parent / unquote(target)).exists():
                errors.append(f'{doc.relative_to(ROOT)}: broken link {target}')
    for error in errors:
        print(error)
    print(f'Checked {len(documents)} documents; {len(errors)} errors')
    return bool(errors)


if __name__ == '__main__':
    raise SystemExit(main())
