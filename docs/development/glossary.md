# Glossário

Termos usados no código e na documentação. Em ordem alfabética. Termos em inglês consagrados ficam como
no original. Onde há um módulo ou ADR com mais detalhe, há um link.

| Termo | Definição |
|---|---|
| **2PL** (*two-phase locking*) | protocolo em que uma transação só **adquire** locks numa 1ª fase e só os **solta** numa 2ª. Na versão *strict* usada aqui, todos os locks caem juntos no fim da transação (commit/rollback). [ADR-0009](../adr/0009-locks-de-tabela-strict-2pl.md) |
| **ACID** | atomicidade, consistência, isolamento, durabilidade: as garantias de uma transação |
| **Admissão** (*admission control*) | decidir **quais** comandos podem começar a executar e **em que ordem**, limitando a concorrência. [`sched`](../modules/sched.md), [ADR-0012](../adr/0012-controle-de-admissao-com-prioridade.md) |
| **Aging** | técnica anti-inanição: quanto mais um comando espera, mais prioridade efetiva ele ganha. Aqui: `chave = nível × passo − chegada` |
| **Ação de topo aninhada** (*nested top action*) | mudança estrutural (alocar uma página) que **sobrevive** ao rollback da transação que a fez. [ADR-0005](../adr/0005-operacoes-estruturais-sobrevivem-ao-rollback.md) |
| **Árvore B+** (*B+tree*) | árvore de busca balanceada em que só as folhas guardam os dados e são encadeadas entre si; boa para intervalos. [`btree`](../modules/btree.md) |
| **Atomicidade** | a transação acontece **inteira ou nada** |
| **Autocommit** | cada comando fora de `BEGIN` é uma transação própria |
| **Batch** (lote) | um registro do WAL: tudo o que **uma** transação commitada fez. [`wal`](../modules/wal.md) |
| **Binding** (*bind*) | trocar nomes de coluna por **posições** na linha, uma vez por comando, antes de avaliar. [`eval`](../modules/eval.md) |
| **Buffer pool** | cache de páginas em memória entre o código e o disco. [`bufferpool`](../modules/bufferpool.md) |
| **Catálogo** | metadados do banco (tabelas, colunas, índices); aqui, uma tabela comum na página 1. [`catalog`](../modules/catalog.md) |
| **Checkpoint** | gravar as páginas sujas no arquivo de dados e **esvaziar** o WAL. *Sharp* = espera todas as transações e bloqueia as novas. [ADR-0002](../adr/0002-no-steal-no-force-wal-redo.md) |
| **Compactação** | reescrever os registros de uma página colados no fim, recuperando o espaço de registros apagados; os slots não mudam. [`heap`](../modules/heap.md) |
| **CRC32** | soma de verificação de 32 bits usada para detectar corrupção/gravação parcial de um registro do WAL |
| **DDL / DML** | linguagem de **definição** (`CREATE`, `DROP`) / de **manipulação** de dados (`INSERT`, `UPDATE`, `DELETE`, `SELECT`) |
| **Deadlock** | ciclo de espera: A espera por B e B espera por A. Detectado por um grafo de espera. [ADR-0010](../adr/0010-vitima-de-deadlock.md) |
| **Dirty page** (página suja) | página em memória modificada que ainda não foi gravada no arquivo de dados |
| **Dirty read** (leitura suja) | ler dados que outra transação **ainda não commitou**; impossível aqui |
| **Durabilidade** | o que foi commitado sobrevive a uma queda |
| **Explain** | mostrar o **plano** de execução de um comando sem executá-lo |
| **Frame** | um espaço do buffer pool que guarda uma página (mais seu latch e bits de controle) |
| **fsync** | forçar os dados do cache do sistema operacional até o dispositivo |
| **Gate** (portão) | `RwLock` que toda transação segura (leitor) e o checkpoint pega (escritor) |
| **Grafo de espera** (*wait-for graph*) | quem espera por quem; um ciclo é um deadlock |
| **Hash join** | junção que monta uma tabela hash com um lado e sonda com o outro |
| **Heap** (arquivo) | conjunto de páginas que guarda as linhas de uma tabela, sem ordem. [`heap`](../modules/heap.md) |
| **Idempotente** | aplicar a operação **uma ou várias vezes** dá o mesmo resultado. Todo `WalOp` é |
| **Index nested loop join** | para cada linha de um lado, **busca no índice** do outro |
| **Índice** | estrutura (aqui, B+tree) que mapeia valor de coluna → `RowId` para buscas rápidas |
| **Inanição** (*starvation*) | um comando/transação que nunca consegue executar porque outros passam sempre na frente |
| **Isolamento** | uma transação não enxerga o trabalho **parcial** das outras |
| **Latch** | trava **curta** e física sobre uma página/estrutura em memória (um `RwLock`). Não confundir com **lock**: o latch protege a estrutura, o lock protege os dados lógicos |
| **Latch crabbing** (*lock coupling*) | descer uma árvore pegando o latch do filho **antes** de soltar o do pai (e soltar os ancestrais quando o filho é "seguro"). [ADR-0006](../adr/0006-btree-latch-crabbing.md) |
| **Lock** (de tabela) | direito lógico sobre uma tabela, **compartilhado** (S) ou **exclusivo** (X), mantido até o fim da transação. [`lockmgr`](../modules/lockmgr.md) |
| **Mark** (marca) | ponto de salvamento dentro de uma transação, para desfazer só o último comando |
| **Memcomparable** | codificação de chaves em que **comparar os bytes** equivale a comparar os valores. [ADR-0007](../adr/0007-chaves-memcomparable.md) |
| **MVCC** | controle de concorrência por múltiplas versões (leitores sem bloqueio). **Não implementado** |
| **No-force** | o commit **não** grava as páginas de dados (só o log) |
| **No-steal** | páginas sujas de transações não commitadas **nunca** vão ao disco (aqui: nenhuma, fora do checkpoint) |
| **Nested loop join** | junção que compara cada linha de um lado com todas do outro |
| **Operador** (Volcano) | nó do plano que produz linhas puxando-as do(s) filho(s) |
| **Página** | unidade de E/S e de cache: 4096 bytes. [`disk`](../modules/disk.md) |
| **Permit** | uma vaga de execução do escalonador, devolvida automaticamente ao sair de escopo (RAII) |
| **Pin** | marca de que alguém está usando uma página; ela não pode ser despejada. Aqui, a contagem do `Arc` |
| **Pushdown** | empurrar um filtro para o mais perto possível do scan da tabela |
| **Redo** | refazer uma operação a partir do log (na recuperação) |
| **Rollback** | desfazer uma transação (ou um comando) |
| **RowId** | endereço físico de uma linha: (página, slot). [`heap`](../modules/heap.md) |
| **Scope** (escopo) | as colunas visíveis numa expressão, em ordem; usado para resolver nomes |
| **Sargável** (*sargable*) | um predicado que **pode** usar um índice (`coluna op constante`) |
| **Savepoint** | veja *Mark* |
| **Session** (sessão) | uma "conexão": executa SQL, mantém a transação aberta e a prioridade. [`db`](../modules/db.md) |
| **Slot** | entrada do vetor de uma *slotted page* que aponta (offset, tamanho) para um registro |
| **Slotted page** | página em que um vetor de slots no início aponta para registros que crescem do fim. [ADR-0001](../adr/0001-paginas-4kb-slotted-pages.md) |
| **Split** (divisão) | dividir um nó cheio em dois |
| **Steal** | permitir gravar páginas sujas de transações não commitadas (exige undo em disco). **Não usado** |
| **Strict 2PL** | veja *2PL* |
| **Tombstone** | marca de "apagado" no lugar do dado. **Não usado** (o slot é liberado na hora) |
| **Torn page** | página gravada **pela metade** numa queda |
| **Transitório** (erro) | falha que a repetição costuma resolver: `Deadlock`, `LockTimeout`, `QueueTimeout` |
| **Txn** | uma transação em andamento (WAL pendente, undo, recursos). [`txn`](../modules/txn.md) |
| **Undo** | desfazer uma operação (aqui, só em memória) |
| **Vítima** (de deadlock) | a transação abortada para destravar um ciclo. [ADR-0010](../adr/0010-vitima-de-deadlock.md) |
| **Volcano** | modelo de execução em que cada operador implementa `next()` e puxa linhas do filho. [ADR-0015](../adr/0015-planejador-por-regras-e-executor-volcano.md) |
| **WAL** (*write-ahead log*) | log gravado **antes** (ou em vez) das páginas, que permite refazer as transações commitadas. [`wal`](../modules/wal.md) |
| **Yield** (ceder a vaga) | devolver a vaga de execução enquanto o comando espera um lock, para não bloquear quem precisa rodar |
