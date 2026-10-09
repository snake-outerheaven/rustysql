# rustysql studio

Aplicativo desktop de exemplo, feito com **[Tauri 2](https://tauri.app)**, para gerenciar um banco **rustysql**. Serve como prova de que a biblioteca é usável de verdade por uma aplicação e como roteiro de como embuti-la.

| Aba | O que faz | API do rustysql usada |
|---|---|---|
| **Console SQL** | executa comandos (`Ctrl+Enter`), mostra tabelas de resultado, tempo e se há transação aberta; `BEGIN` fica aberto entre um comando e outro; botão **EXPLAIN**; seletor de prioridade | `Session::execute`, `Session::set_priority`, `Session::in_transaction` |
| **Dados** | navega pelas linhas de qualquer tabela, com paginação | `Database::run` (`SELECT … LIMIT … OFFSET`) |
| **Monitor** | páginas sujas, tamanho do WAL, fila e admissões do gerenciador de prioridade, locks de tabela, atualizados a cada segundo | `dirty_pages`, `wal_bytes`, `scheduler_stats`, `lock_snapshot` |
| **Laboratório** | N threads fazem transferências concorrentes (uma em prioridade ALTA, uma em BAIXA), tratam deadlocks com nova tentativa e conferem que a soma dos saldos não mudou | `Database::session`, `Error::Deadlock` |

Na barra lateral: abrir/criar um arquivo `.db`, ver tabelas, colunas e índices, carregar o `demo.sql` (2000 linhas) e forçar um **checkpoint**.

## Como rodar

Pré-requisitos: Rust 1.88+ e, no Windows, o WebView2 (já vem no Windows 11). Não precisa de Node.

```bash
cd studio
cargo run --release
```

Em `cargo run` sem `--release` o banco também funciona, só fica mais lento nos 2000 inserts da demo.

## Como é feito

```
studio/
├── Cargo.toml        crate independente (tem o próprio [workspace]); depende de ../ por path
├── tauri.conf.json   janela, CSP restrita, sem bundler de frontend
├── capabilities/     permissões da janela (só core:default)
├── src/main.rs       comandos Tauri: um por ação da interface
└── ui/               HTML + CSS + JavaScript puro (sem npm, sem build)
```

- O estado do app é um `Database` (barato de clonar, `Send + Sync`) e **uma** `Session` para o console, guardada num `Mutex`; é ela que mantém uma transação aberta entre comandos.
- Os comandos são `async` e rodam a biblioteca em `spawn_blocking`, para que um lock demorado ou a fila de admissão nunca congelem a interface.
- O nome de tabela que vem da interface só é aceito se estiver em `table_names()`; o resto do SQL é do usuário, que está operando o próprio banco.
- `Database` faz o checkpoint final ao ser solto: **Fechar** ou sair do app deixa o arquivo consistente.

> Este app é uma demonstração. Ele não tem autenticação nem isolamento entre "clientes": quem abre a janela tem acesso total ao arquivo.
