# Convenções de código

Regras que o código do rustysql já segue e que as contribuições devem manter. Onde há uma verificação
automática, ela é citada.

## Verificações automáticas (devem passar)

```bash
cargo build                  # sem avisos
cargo clippy --all-targets   # sem avisos
cargo doc --no-deps          # sem avisos (links quebrados de rustdoc são avisos)
cargo test                   # 52 testes
```

O crate declara `#![warn(missing_docs)]` e `#![warn(rustdoc::broken_intra_doc_links)]` em `lib.rs`:
**todo item público precisa de documentação**.

## Documentação

### No código (rustdoc)

* **Todo módulo** começa com `//!`: o que faz, por que existe, e (quando não óbvio) o que **não** faz.
* **Todo item público** tem `///`. Primeira linha: **uma frase** que diz o que é/faz. Depois, se
  necessário, o detalhe.
* Seções padrão, quando aplicáveis, na ordem:

  | Seção | Quando |
  |---|---|
  | `# Errors` | a função devolve `Result` e há erros que o chamador deve conhecer |
  | `# Panics` | pode dar `panic!` |
  | `# Concorrência` | há uma regra de latch/lock que o chamador precisa respeitar |
  | `# Exemplo` | um exemplo curto e **executável** (vira teste com `cargo test --doc`) |

* Comentários explicam o **porquê**, não o quê. Se o comentário descreve o que a linha faz, melhor
  renomear. Se descreve uma decisão não óbvia, referencie o ADR (`ADR-0005`).
* A documentação **é escrita em português**; identificadores de código, em inglês.
* A doc vem **antes** dos atributos: `/// ...` e depois `#[derive(...)]`.
* Os campos de um `enum` com variantes de struct são documentados **um por linha** (rustfmt-friendly).

### Em `docs/`

* Mudou comportamento, formato em disco ou uma decisão? Atualize **o módulo, a referência e o ADR**
  correspondente (ou escreva um ADR novo: nunca reescreva um aceito).
* Números e saídas citados na documentação devem ser **reproduzíveis**: rode e cole (`cargo run --example
  storage_dump`, `EXPLAIN`...). Não invente saída.
* Links relativos entre documentos; confira com o verificador de links (veja o `CONTRIBUTING.md`).

## Estilo de código

| Tema | Regra |
|---|---|
| Edição | Rust 2024; `let ... else` e *let chains* são bem-vindos |
| `unsafe` | **zero** no projeto. Se algo exige, precisa de ADR |
| `unwrap()` / `expect()` | só quando o invariante é **local e óbvio** (ex.: tamanho de slice já checado). `expect("mensagem do invariante")` em vez de `unwrap()` quando o motivo não é óbvio |
| `panic!` | só para violação de invariante interno (bug), nunca para erro de usuário ou de dados |
| Nomes | tipos `CamelCase`, funções `snake_case`; **ids** de `PageId`/`TableId`/`TxnId` têm *type alias*, não `u32` solto |
| Comprimento | funções curtas; extraia helpers com nome que explique (`has_room`, `reserve`, `key_for`) |
| Idioma de identificadores | inglês; **mensagens ao usuário**, em português |
| `pub` | o mínimo. Módulos são `pub` para permitir testes por camada, mas o que é detalhe interno é `pub(crate)` ou privado (ex.: `Inner`) |

## Tratamento de erros

1. **Um** tipo: `rustysql::Error`; **nunca** `String` nem `Box<dyn Error>` na API.
2. Erros de **usuário** (SQL, dados, restrição) → `Error::Parse`/`Error::Exec`, com mensagem acionável
   (diga o **valor** e o **nome**: `coluna 'x' é INT, mas recebeu ...`).
3. Inconsistência **interna** ou do arquivo → `Error::Storage`.
4. `?` para propagar; **não engula** erros (`let _ =` só em limpeza que não pode falhar o chamador, como
   o `ROLLBACK` depois de um erro, e com comentário).
5. Falhas **transitórias** são variantes próprias (`Deadlock`, `LockTimeout`, `QueueTimeout`): o chamador
   precisa distingui-las sem comparar texto.
6. Todo comando que falha deve deixar o banco **como estava** (atomicidade por comando): se você
   acrescentar um efeito colateral, registre o **undo** (`txn.undo`) *antes* de seguir.

## Regras de concorrência

Estas regras evitam deadlocks e corrupção; **quebrá-las exige um ADR**.

1. **Ordem de aquisição, de fora para dentro:**
   `portão de checkpoint → vaga de execução (Permit) → locks de tabela (em ordem de id) → latches de página`.
2. **Latches de página: sempre de cima para baixo** na árvore e da esquerda para a direita nas folhas.
   Pegue o do filho **antes** de soltar o do pai.
3. **Nunca espere** (nem faça E/S pesada) segurando um mutex "curto" (`state` do pool, do escalonador, do
   gerenciador de locks). Exceção documentada: a leitura de uma página do disco no `fetch`.
4. **O gerenciador de locks nunca *espera* o escalonador** (só pode **liberar** uma vaga via `on_block`) e o
   escalonador nunca chama o gerenciador de locks.
5. **Um `PageRef` (pin) mantém a página no pool**; não guarde guards de latch além do necessário.
6. **Estado compartilhado é `Send + Sync` por construção**: prefira `Arc<Mutex/RwLock<_>>` e atômicos a
   `unsafe`. Guards que precisam ser guardados em vetores ou movidos usam o `parking_lot` com `arc_lock`.
7. Toda nova operação de B-tree é testada **com concorrência** e validada por **mutação** (veja
   [`testing.md`](testing.md#teste-de-mutação-manual)).

## Persistência e formato

* Mudou o formato de página, de linha, de chave ou de WAL? **Troque o *magic*** (`RUSTYSQ2` → `RUSTYSQ3`)
  e atualize [`storage-formats.md`](../reference/storage-formats.md) **no mesmo commit**. Não há migração:
  arquivos antigos são recusados com uma mensagem clara.
* Todo `WalOp` novo precisa ser **idempotente** e ter um caso em `heap::apply_op`.
* O WAL **só** guarda operações de heap; índices são reconstruídos (ADR-0004).

## Testes

Veja [`testing.md`](testing.md). Em resumo: teste a **propriedade**, prove que o teste **pode falhar**,
nada de `sleep` para "esperar a corrida", arquivos temporários **únicos**.

## Commits

* Mensagens em português, no imperativo ou descritivas, com um **resumo** de até ~70 caracteres, linha
  em branco e um corpo que explica o **porquê** (e as medidas, quando há desempenho).
* **Um assunto por commit** (um módulo, uma decisão). Não misture formatação com mudança de lógica.
* Se o trabalho teve ajuda de IA, mantenha o *trailer* `Co-Authored-By: ...` do autor assistente. Ele
  separa o que foi escrito com apoio de IA do que não foi.
* O histórico de decisões fica nos **ADRs**, não nas mensagens de commit.

## Checklist de revisão

- [ ] compila sem avisos; `clippy` limpo; `cargo doc` sem avisos
- [ ] todo item público novo tem rustdoc (com exemplo se for API de entrada)
- [ ] testes novos **falham sem a mudança** (mutação) e passam com ela
- [ ] se mexeu em concorrência: rodou a suíte 20× em `--release`
- [ ] se mexeu em formato: *magic* trocado e `storage-formats.md` atualizado
- [ ] se tomou uma decisão de arquitetura: ADR novo (ou ADR existente marcado como substituído)
- [ ] a documentação afetada (módulo, referência, guia) foi atualizada
