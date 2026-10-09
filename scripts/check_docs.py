#!/usr/bin/env python3
"""Verifica os links relativos e as âncoras de todos os arquivos Markdown do repositório.

Uso:  python scripts/check_docs.py

* links `[texto](caminho)`: o arquivo/pasta precisa existir;
* links `[texto](caminho#ancora)` e `[texto](#ancora)`: a âncora precisa existir no arquivo de destino
  (calculada como o GitHub faz: minúsculas, sem pontuação, espaços viram hífens);
* links externos (`http://`, `https://`, `mailto:`) são ignorados (não há rede aqui);
* o que está dentro de blocos de código (``` ... ```) e de `código inline` é ignorado.

Código de saída: 0 se tudo certo; 1 se houver links quebrados.
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SKIP_DIRS = {"target", ".git", "node_modules"}

LINK = re.compile(r"\[(?:[^\]\[]|\[[^\]]*\])*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$")


def slug(heading: str) -> str:
    """Âncora de um título, no estilo do GitHub."""
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", heading)   # [texto](url) -> texto
    text = text.replace("`", "").replace("*", "").replace("~", "")
    text = re.sub(r"<[^>]+>", "", text)                       # tags html
    text = text.strip().lower()
    text = re.sub(r"[^\w\- ]", "", text)                      # \w inclui letras unicode e "_"
    return text.replace(" ", "-")


def strip_code(lines, inline=True):
    """Gera (n, linha) sem blocos de código e, se `inline`, sem trechos `inline`.

    Para ler títulos, use `inline=False`: o texto entre crases faz parte da âncora do título.
    """
    in_fence = False
    for n, line in enumerate(lines, 1):
        if re.match(r"^\s*(```|~~~)", line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        yield n, (re.sub(r"`[^`]*`", "", line) if inline else line)


_anchor_cache = {}


def anchors_of(path: Path) -> set:
    if path in _anchor_cache:
        return _anchor_cache[path]
    seen, result = {}, set()
    for _, line in strip_code(path.read_text(encoding="utf-8").splitlines(), inline=False):
        m = HEADING.match(line)
        if not m:
            continue
        base = slug(m.group(2))
        count = seen.get(base, 0)
        seen[base] = count + 1
        result.add(base if count == 0 else f"{base}-{count}")
    _anchor_cache[path] = result
    return result


def main() -> int:
    # consoles do Windows costumam ser cp1252: não deixe um acento derrubar o relatório
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    files =[p for p in ROOT.rglob("*.md") if not (set(p.relative_to(ROOT).parts) & SKIP_DIRS)]
    errors, checked = [], 0
    for md in sorted(files):
        lines = md.read_text(encoding="utf-8").splitlines()
        for n, line in strip_code(lines):
            for target in LINK.findall(line):
                if re.match(r"^(https?:|mailto:)", target):
                    continue
                checked += 1
                path_part, _, anchor = target.partition("#")
                dest = md if path_part == "" else (md.parent / path_part).resolve()
                rel = md.relative_to(ROOT)
                if not dest.exists():
                    errors.append(f"{rel}:{n}: arquivo inexistente: {target}")
                    continue
                if anchor and dest.suffix == ".md" and dest.is_file():
                    if anchor not in anchors_of(dest):
                        errors.append(f"{rel}:{n}: âncora inexistente: {target}")
    print(f"{len(files)} arquivos Markdown, {checked} links relativos verificados")
    for e in errors:
        print("  ERRO:", e)
    print("OK" if not errors else f"{len(errors)} problema(s)")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
