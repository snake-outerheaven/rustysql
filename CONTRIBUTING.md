# Contribuindo com o rustysql

Obrigado pelo interesse! O rustysql é um banco de dados escrito **para aprender**; contribuições que
tornam o código mais claro, mais correto ou melhor documentado são muito bem-vindas.

## Antes de começar

1. Leia o [`README`](README.md) e a [arquitetura](docs/architecture/arc42.md) (30 minutos bastam).
2. Para mexer num módulo, leia a [página dele](docs/modules/README.md) e os **ADRs** relacionados:
   várias decisões que parecem estranhas têm um motivo registrado
   ([índice de ADRs](docs/adr/README.md)).
3. Veja as [convenções](docs/development/conventions.md) e a [estratégia de testes](docs/development/testing.md).

## Preparando o ambiente

```bash
git clone https://github.com/snake-outerheaven/rustysql.git
cd rustysql
cargo test                 # deve passar tudo (52 testes)
cargo run --release -- demo.db demo.sql   # um passeio rápido
```

Requisitos: **Rust 1.88+** (edição 2024; o código usa *let chains*). Para o verificador de documentação, Python 3.

## O fluxo

1. **Abra uma *issue*** (ou comente numa existente) descrevendo o problema ou a ideia, principalmente se
   for uma mudança de **arquitetura**: ela provavelmente precisa de um [ADR](docs/adr/README.md).
2. Crie um *branch* a partir de `master`.
3. Faça a mudança **com testes** (veja abaixo).
4. Rode a lista de verificação.
5. Abra o *pull request* explicando **o porquê** (o *quê* o diff já mostra).

## Lista de verificação

```bash
cargo build                    # sem avisos
cargo clippy --all-targets     # sem avisos
cargo test                     # tudo verde
cargo doc --no-deps            # sem avisos (links de rustdoc quebrados são avisos)
python scripts/check_docs.py   # nenhum link ou âncora quebrados na documentação
```

Se a mudança mexe em **concorrência** (latches, locks, escalonador), rode também a suíte concorrente várias
vezes, em modo *release* e com limite de tempo, para caçar instabilidade e travamentos:

```bash
for i in $(seq 1 20); do timeout 120 cargo test --release --test btree --test txn --test sched --test recovery || break; done
```

## O que vale uma contribuição

| Tipo | Observações |
|---|---|
| **Correção de bug** | acrescente um teste que **falha antes** e passa depois (e cite o bug no comentário do teste) |
| **Teste novo** | prove que ele **pode falhar** (mutação: quebre o código de propósito). Veja [`testing.md`](docs/development/testing.md#teste-de-mutação-manual) |
| **Documentação** | muito bem-vinda: erros, exemplos, diagramas. Saídas citadas devem ser **reproduzíveis** |
| **Funcionalidade SQL** | atualize [`docs/reference/sql.md`](docs/reference/sql.md) e acrescente testes em `tests/sql.rs` |
| **Mudança de formato em disco** | troque o *magic* e atualize [`storage-formats.md`](docs/reference/storage-formats.md) **no mesmo commit** |
| **Decisão de arquitetura** | um [ADR novo](docs/adr/README.md#modelo-para-novos-adrs) (nunca reescreva um ADR aceito: marque-o como substituído) |
| **Desempenho** | traga **medidas** (antes/depois); não otimize sem número |

### Ideias para começar

A lista de dívidas está na [seção 11 do arc42](docs/architecture/arc42.md#11-riscos-e-dívida-técnica). Algumas
boas portas de entrada:

* **fusão de nós** na B-tree ([ADR-0008](docs/adr/0008-raiz-fixa-e-delete-sem-fusao.md));
* **lista de páginas livres** (reaproveitar as páginas de `DROP TABLE`);
* ***group commit*** (vários commits num só `fsync`);
* funções escalares (`UPPER`, `COALESCE`...) e `DISTINCT`;
* *fuzzing* do parser e testes baseados em propriedades (lacunas conhecidas em
  [`testing.md`](docs/development/testing.md#lacunas-conhecidas)).

## Estilo e convenções (resumo)

* **Sem `unsafe`.**
* **Todo item público é documentado** (`#![warn(missing_docs)]`); comentários explicam o **porquê**.
* Mensagens de erro e documentação em **português**; identificadores em **inglês**.
* Erros de usuário são `Error::Exec`/`Parse` (com mensagem acionável); nunca `panic!` por entrada do usuário.
* Respeite a **ordem de aquisição** de recursos (portão → vaga → locks → latches): veja
  [`conventions.md`](docs/development/conventions.md#regras-de-concorrência).

## Mensagens de commit

Português, com um resumo curto e um corpo que explica o **porquê**. Um assunto por commit. Trabalhos feitos
com ajuda de IA mantêm o *trailer* `Co-Authored-By:` do autor assistente.

## Conduta

Seja gentil e objetivo. Críticas são ao código, não às pessoas. Dúvidas de iniciante são bem-vindas: este é
um projeto de estudo.

## Licença

O projeto é distribuído sob a [licença MIT](LICENSE). Ao enviar uma contribuição, você concorda que ela
seja licenciada sob os mesmos termos (o que permite, entre outros usos, o uso comercial).
