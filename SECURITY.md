# Política de segurança

## Aviso importante

O rustysql é um **projeto de estudo**. Ele **não foi auditado** e não deve guardar dados sensíveis ou
críticos sem que você mesmo avalie os riscos. Veja os [limites conhecidos](README.md#limites-conhecidos).

## Versões com suporte

Só a versão mais recente da branch `master` recebe correções.

## Como relatar uma vulnerabilidade

**Não abra uma issue pública** para vulnerabilidades. Use o relato privado do GitHub:

1. Vá em [**Security → Report a vulnerability**](https://github.com/snake-outerheaven/rustysql/security/advisories/new).
2. Descreva o problema, como reproduzi-lo e o impacto que você vê.

Respondo assim que possível (é um projeto mantido por uma pessoa, sem prazo garantido). Depois da correção,
o relato é publicado com o crédito a quem o encontrou, se você quiser.

## O que interessa

* corrupção de dados ou perda de transações commitadas;
* leitura de memória/arquivo fora do esperado a partir de um arquivo `.db`/`.wal` malformado;
* *panics* ou travamentos provocados por entrada SQL maliciosa;
* quebra de isolamento entre sessões.

Não há `unsafe` no código do projeto (`grep unsafe src/` não devolve nada), então vulnerabilidades de
memória viriam de dependências.

## O que **não** é uma vulnerabilidade

* Injeção de SQL na aplicação que usa a biblioteca: **não há *prepared statements***; escapar as aspas é
  responsabilidade de quem monta o SQL (veja o [guia da API](docs/guides/embedding-api.md#parâmetros-e-injeção-de-sql)).
* Limites documentados (tamanho de transação, locks de tabela, ausência de autenticação).
