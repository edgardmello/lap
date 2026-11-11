# Filtro da biblioteca e palavras-chave

Implementação local inspirada no fluxo de organização do Lightroom Classic. Referências funcionais: [filtros da biblioteca](https://helpx.adobe.com/lightroom-classic/desktop/organize-photos-in-lightroom-classic/finding-photos-catalog.html) e [palavras-chave e sugestões](https://helpx.adobe.com/lightroom-classic/desktop/organize-photos-in-lightroom-classic/keywords.html).

## Uso

Na barra superior da área de fotos, os novos botões de filtro e tag abrem **Filtro da biblioteca** e **Palavras-chave**.

O filtro combina três seções que podem ficar abertas simultaneamente:

- **Texto:** nome do arquivo, palavras-chave, descrição/comentários ou os campos pesquisáveis em conjunto. Modos: todas as palavras, qualquer palavra, frase, não contém, começa com e termina com. `%`, `_` e `\` são pesquisados literalmente.
- **Atributos:** classificação mínima, sinalizador de revisão, favoritas e rótulos de cor.
- **Metadados:** ano de captura, câmera, lente, rótulo, palavra-chave e tipo de arquivo. Clique para escolher um valor; ⌘/Ctrl/Shift + clique adiciona ou retira valores na mesma coluna. Critérios de colunas diferentes são combinados com **E**; valores da mesma coluna com **OU**.

Cada coluna calcula suas opções respeitando os outros filtros, mas ignorando sua própria seleção. Assim é possível adicionar outra câmera sem perder as opções da coluna. Os totais contam arquivos distintos, mesmo quando uma foto possui várias palavras-chave. Há opções explícitas para valores desconhecidos e fotos sem palavras-chave. **Nenhum** limpa os critérios; fechar a barra mantém os filtros aplicados. Os filtros são temporários e são limpos ao mudar de biblioteca.

O painel de palavras-chave opera sobre a foto ativa ou toda a seleção múltipla, incluindo IDs selecionados fora da página carregada. Ele oferece:

- Entrada de várias palavras-chave separadas por vírgulas, ponto e vírgula ou quebra de linha.
- Hierarquia pela notação `Imóveis > Imóvel 1 > Sala de estar`.
- Indicação de palavras-chave presentes em apenas parte da seleção.
- Sugestões locais baseadas em palavras-chave compartilhadas, pasta e proximidade da data de captura.
- Lista pesquisável, organizada pelos grupos de tags existentes, com contagens, expansão de hierarquia, aplicação por checkbox e seta para filtrar as fotos.
- Aplicação e remoção de rótulos de cor na seleção.

As palavras-chave reutilizam `atags` e `afile_tags`. Os caminhos hierárquicos são armazenados como nomes completos, com o separador ` > `; os ancestrais são criados sem duplicação e passam a incluir fotos dos descendentes nas contagens e no filtro novo. A remoção de um ancestral pelo novo painel remove suas associações e as dos descendentes da seleção. A tela de tags já existente continua disponível e mantém sua semântica anterior de correspondência exata. As sugestões são determinísticas e locais, sem chamada de IA ou serviço externo.

## Persistência e integração

A migração **21** cria `afile_labels`, com cinco cores permitidas, índice por cor e exclusão em cascata quando um registro de foto é removido. Não reescreve registros de fotos ou palavras-chave existentes. Palavras-chave e rótulos são gravados no **catálogo do LAP**; esta implementação não escreve XMP/IPTC nos arquivos originais e não importa ou exporta catálogos do Lightroom.

`libraryFilter` é um campo opcional, com valor padrão vazio, de `QueryParams` e `SmartQueryParams`. Os mesmos predicados SQL são usados pelos resultados, páginas, contagens, IDs, posição de arquivos, linhas agrupadas e linhas do tempo. Coleções mantêm seu escopo. Filtros de álbuns, pastas excluídas e vídeos auxiliares de Live Photos continuam sendo respeitados. As regras de álbuns inteligentes continuam agrupadas corretamente antes de aplicar os filtros adicionais.

Resultados temporários de busca semântica não são filtrados no cliente; a barra informa que os critérios se aplicam às consultas do catálogo. Usar a seta da lista de palavras-chave a partir desses resultados retorna à biblioteca.

Os totais originais da barra lateral não são substituídos pelas contagens de uma consulta temporariamente filtrada. Operações de criação e aplicação de palavras-chave são transacionais; uma falha reverte a operação inteira.

## Verificação

- Build de produção do frontend e compilação Rust de desenvolvimento aprovados.
- Suíte Rust completa: **206 aprovados, 1 ignorado**, incluindo seis testes do novo módulo. Verifica combinação de critérios, busca literal, contagens e escopo de coleção, hierarquia/deduplicação/transação, sugestões, regras inteligentes e migração de cores.
- Suíte frontend `.test.mjs`: **148 aprovados**, incluindo seleção de facetas, normalização de caminhos e verificação dos bindings dos templates.
- Revisão visual dos componentes em prévia isolada com a ponte Tauri simulada: seis colunas, múltipla seleção, sugestões, adição de termo hierárquico e seta da lista. A consulta real e as gravações foram exercitadas nos testes Rust com SQLite em memória.
- Não foi substituído o aplicativo instalado nem efetuada uma migração no catálogo de produção para testar a interface. O executável de desenvolvimento precisa ser reaberto para carregar os novos comandos.
