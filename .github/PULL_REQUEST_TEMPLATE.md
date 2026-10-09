## O que muda e por quê

<!-- O "porquê" importa mais que o "quê": o diff já mostra o quê. Cite a issue: "Fecha #123". -->

## Como foi testado

<!-- Que testes foram acrescentados? Eles FALHAM sem a mudança? (veja docs/development/testing.md) -->

## Lista de verificação

- [ ] `cargo build` e `cargo clippy --all-targets` sem avisos
- [ ] `cargo test` passa
- [ ] `cargo doc --no-deps` sem avisos; itens públicos novos têm rustdoc
- [ ] `python scripts/check_docs.py` passa
- [ ] mexe em **concorrência**? rodei a suíte concorrente 20× em `--release`
- [ ] mexe em **formato de disco**? troquei o *magic* e atualizei `docs/reference/storage-formats.md`
- [ ] tomei uma **decisão de arquitetura**? escrevi um ADR (`docs/adr/`)
- [ ] atualizei a documentação afetada (módulo, referência, guia) e o `CHANGELOG.md`
