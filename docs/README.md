# Documentação do rustysql

Este diretório é a documentação técnica completa do **rustysql**, um banco de dados relacional embutido
escrito em Rust para estudo. Ela segue modelos consagrados na indústria, cada um para o seu fim:

| Modelo | Onde está | Para que serve |
|---|---|---|
| [arc42](https://arc42.org) | [`architecture/arc42.md`](architecture/arc42.md) | descrição da arquitetura em 12 seções padronizadas |
| [C4 model](https://c4model.com) | [`architecture/c4.md`](architecture/c4.md) | diagramas de contexto, contêineres e componentes |
| Cenários de execução (UML de sequência) | [`architecture/runtime-scenarios.md`](architecture/runtime-scenarios.md) | o que acontece, passo a passo, em cada fluxo importante |
| [MADR](https://adr.github.io/madr/) (ADRs) | [`adr/`](adr/README.md) | **por que** cada decisão foi tomada, e o que ela custou |
| Referência de módulo | [`modules/`](modules/README.md) | um documento por módulo do código-fonte |
| Documentação de referência | [`reference/`](reference/) | SQL, formatos binários, erros, configuração, CLI |
| Guias (*how-to*) | [`guides/`](guides/) | tarefas: começar, embutir, tunar, operar |
| Documentação de desenvolvimento | [`development/`](development/) | testes, convenções, glossário |
| [Keep a Changelog](https://keepachangelog.com) | [`../CHANGELOG.md`](../CHANGELOG.md) | histórico de mudanças |
| rustdoc | `cargo doc --no-deps --open` | referência de API gerada do código (todo item público é documentado) |

> **Regra de ouro:** a documentação descreve o que o código *faz*; quando os dois divergem, o código
> está certo e a documentação tem um bug. Os números citados (constantes, valores padrão, nomes de
> testes) foram extraídos do código-fonte.

## Por onde começar

| Eu quero... | Leia |
|---|---|
| **usar** o banco pelo terminal | [`guides/getting-started.md`](guides/getting-started.md) → [`reference/cli.md`](reference/cli.md) → [`reference/sql.md`](reference/sql.md) |
| **embutir** o banco num programa Rust | [`guides/embedding-api.md`](guides/embedding-api.md) → [`guides/concurrency-and-transactions.md`](guides/concurrency-and-transactions.md) |
| **entender a arquitetura** | [`architecture/arc42.md`](architecture/arc42.md) → [`architecture/c4.md`](architecture/c4.md) → [`architecture/runtime-scenarios.md`](architecture/runtime-scenarios.md) |
| entender **por que** algo é assim | [`adr/`](adr/README.md) |
| **estudar** uma peça específica | [`modules/`](modules/README.md) (índice com o grafo de dependências) |
| **ajustar o desempenho** | [`guides/query-tuning.md`](guides/query-tuning.md) → [`reference/configuration.md`](reference/configuration.md) |
| **operar** / diagnosticar um banco | [`guides/operations-runbook.md`](guides/operations-runbook.md) → [`reference/errors.md`](reference/errors.md) |
| **contribuir** | [`../CONTRIBUTING.md`](../CONTRIBUTING.md) → [`development/conventions.md`](development/conventions.md) → [`development/testing.md`](development/testing.md) |
| um termo que não conheço | [`development/glossary.md`](development/glossary.md) |

## Mapa do repositório

```
rustysql/
├── src/                  código-fonte (um arquivo por módulo; veja docs/modules)
├── tests/                testes de integração (SQL, B-tree, transações, escalonador, recuperação)
├── docs/                 esta documentação
│   ├── architecture/     arc42, C4, cenários de execução
│   ├── adr/              registros de decisão de arquitetura
│   ├── modules/          referência por módulo
│   ├── reference/        SQL, formatos, erros, configuração, CLI
│   ├── guides/           guias práticos
│   └── development/      testes, convenções, glossário
├── demo.sql              roteiro de demonstração (cargo run -- demo.db demo.sql)
├── CONTRIBUTING.md
└── CHANGELOG.md
```

## Convenções desta documentação

* **Idioma**: português do Brasil; identificadores de código e termos consagrados em inglês
  (*latch*, *pin*, *checkpoint*, *write-ahead log*) ficam como no original.
* **Diagramas**: [Mermaid](https://mermaid.js.org), renderizado pelo GitHub e pela maioria dos editores.
* **Níveis de certeza**: afirmações marcadas **Garantia** são verificadas por teste automatizado
  (o teste é citado); afirmações marcadas **Intenção** descrevem o desenho, mas não têm teste
  dedicado; **Limitação** descreve o que o sistema deliberadamente não faz.
* **Caminhos**: relativos à raiz do repositório, salvo indicação.

## Estado do projeto

| Aspecto | Situação |
|---|---|
| Código | ~6,4 mil linhas em `src/` (incluindo documentação de API), 21 módulos |
| Testes | 1 unitário + 44 de integração + 7 exemplos executáveis (rustdoc) |
| Dependências de runtime | uma: [`parking_lot`](https://crates.io/crates/parking_lot) (guards de lock "owned") |
| Maturidade | projeto de estudo; **não** use para dados que importam |
