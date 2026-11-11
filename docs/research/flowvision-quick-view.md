# FlowVision e visualização rápida no LAP

Investigação e implementação local em 11/11/2026.

## Resultado

O LAP ganhou uma janela **Quick view / Visualização rápida**, acessível pelo botão de fotos acima de Configurações. Ela recebe imagens locais ou pastas, mantém os caminhos e a seleção em memória, gera miniaturas sob demanda e oferece os arquivos originais para copiar ou arrastar. A sessão não importa imagens, cria álbuns, registra arquivos ou usa o cache de miniaturas do catálogo. Fechar a janela descarta a sessão.

O aplicativo LAP continua inicializando seu banco normalmente ao abrir. O isolamento se aplica ao novo fluxo de visualização: a janela `quickview` pula `libConfig.init()` e seus três comandos não consultam nem escrevem no SQLite. Não foi criado um executável separado que dispense o banco em toda a aplicação.

## Identidade e método

- Alvo: `/Applications/FlowVision.app`, versão **1.7.6**, build **20260908**.
- Executável universal SHA-256: `0896ebdb7c7fd0c619adf415edf5ca8f73b80cff36a25cd37c7487ce14eb801f`.
- Fatias: x86_64 e arm64. Fatia arm64 extraída com `lipo`, SHA-256 `f46a24d90522ab0a0b5855a022c0ff70b7d29021ead237e7e401e1b9d90829ab`.
- REA CLI **6.5.0**, já disponível localmente: inspeção Mach-O com evidência `ev_3bab7c10c27c453192e543ee6bf25aac599bf14899bee27d84d340321c55f249`.
- Ghidra **12.2** local, linguagem `AARCH64:LE:64:AppleSilicon:default`: importação, análise automática completa e descompilação de entradas Objective-C relevantes.
- Uma segunda passagem sem análise automática descompilou cinco funções internas identificadas na primeira. Essa passagem tem cobertura local, sem a tipagem e o grafo global da primeira.

A ponte REA conectada não tinha `GHIDRA_INSTALL_DIR` e sua linha aceita é 12.1.x; o Ghidra instalado é 12.2. Em vez de alterar configuração ou instalar outra versão, foram combinadas a inspeção do REA e a execução direta do Ghidra. Os diretórios de configuração, cache e projetos temporários do Ghidra ficaram em `/tmp/lap-flowvision-analysis`. Os projetos temporários foram descartados pelo próprio Ghidra. O executável original não foi modificado.

Evidências e scripts reproduzíveis estão em [flowvision-quick-view-evidence](./flowvision-quick-view-evidence/). `sha256.json` identifica cada arquivo. O pseudocódigo é saída do descompilador, não código-fonte original. Os arquivos de texto tiveram apenas espaços ao fim das linhas e linhas vazias finais normalizados para publicação.

## Achados com cobertura delimitada

| Questão | Observação | Evidência | Situação |
| --- | --- | --- | --- |
| Como o grid fornece itens? | `CustomCollectionViewManager` implementa `collectionView:itemForRepresentedObjectAtIndexPath:` e delega a uma função interna. Há classes `CustomCollectionView`, `CustomCollectionViewItem` e `FileModel` nos metadados. | `ghidra-findings.txt`, entrada `0x100134ca4` e inventário `MATCHING_SYMBOLS` | Respondida quanto à estrutura nativa; layout interno completo não foi reconstruído. |
| Como um item é arrastado? | `collectionView:pasteboardWriterForItemAtIndexPath:` delega a `0x100136024`. Essa função cria um item e escreve `URL.absoluteString` com o tipo `NSPasteboardTypeFileURL`. | `ghidra-findings.txt`, `0x100134ee4`; `ghidra-focused.txt`, `0x100136024` | Respondida estaticamente para o escritor por item. |
| Como funciona Copiar? | `AppDelegate::editCopy:` preserva a cópia de campos de texto; para o grid, limpa o pasteboard geral, obtém uma coleção, faz a ponte para `NSArray` e chama `NSPasteboard::writeObjects:`. | `ghidra-findings.txt`, `0x10014a03c`; auxiliares `0x100030eb8` e `0x10017d908` em `ghidra-focused.txt` | Respondida para o despacho e publicação da cópia; nem todos os ramos da seleção foram reconstruídos. |
| Como recebe drops? | Há implementações próprias de `draggingEntered:` e `performDragOperation:` em `CoreAreaView`, `CustomImageView` e `CustomPathControl`. | `ghidra-findings.txt`, `0x10012c258`, `0x10012c828`, `0x10001f4a0`, `0x10001fa90`; auxiliar `0x10012c2b0` em `ghidra-focused.txt` | Parcialmente respondida. Não se presume que o drop do FlowVision seja temporário ou sem efeitos persistentes. |
| Como gera e mantém miniaturas? | O binário importa `CGImageSourceCreateThumbnailAtIndex` e opções de tamanho/transformação. Há símbolos e strings de `SDImageCache` e estado `FileModel::getThumbFailed`. | REA `rea-macho.json` para imports; inventário Ghidra para nomes | Parcialmente respondida: são capacidades presentes, não prova de execução em cada grid. Política completa de cache não foi reconstruída. |
| O FlowVision inteiro dispensa banco? | Esta investigação não estabelece essa afirmação. | Limite do escopo e das funções examinadas | Não resolvida; desnecessária para o recurso do LAP. |

Não houve reprodução instrumentada de arrastes no FlowVision. O Ghidra reportou limitações de análise de algumas instruções e bibliotecas de sistema não carregadas; não se trata de reconstrução integral ou equivalência funcional.

## Decisões no LAP

Implementação própria em Vue/Tauri/Rust, reaproveitando os decodificadores e a integração nativa existentes do LAP:

- `src-vite/src/views/QuickView.vue`: sessão, drop, seleção, preview, copiar e arraste.
- `src-vite/src/components/QuickViewTile.vue`: carregamento próximo ao viewport, descarte da miniatura fora dele e estado de falha.
- `src-vite/src/common/quickView.ts`: deduplicação e seleção por intervalo.
- `src-vite/src/common/quickViewWindow.ts`: janela própria, reutilização e `dragDropEnabled: false` para receber drops DOM.
- `src-tauri/src/t_quick_view.rs`: leitura de arquivos/pastas, geração direta de miniaturas e cópia de originais.

O fluxo evita `thumb://`, `preview://` e os importadores do catálogo, que dependem do SQLite. A janela própria também evita os listeners de drop dos álbuns e da área principal.

Comandos:

| Comando | Entrada | Saída |
| --- | --- | --- |
| `quick_view_collect` | `paths: string[]` | `files: {path,name,size}[]`, `errors`, `truncated` |
| `quick_view_thumbnail` | `path: string`, `size?: number` | `dataUrl` |
| `quick_view_copy` | `paths: string[]` | Quantidade copiada |

Limites: 1.000 imagens por sessão, 20.000 entradas examinadas por coleta e quatro decodificações simultâneas. Pastas são percorridas recursivamente sem seguir links simbólicos de diretório. Canonicalização deduplica aliases. JPEG, PNG e demais imagens suportadas, HEIF e RAW usam os decodificadores existentes, com orientação EXIF. Os comandos não gravam miniaturas no cache de disco; decodificadores internos de alguns formatos podem usar arquivos temporários.

A cópia oferece URLs dos arquivos, sem converter a seleção em pixels de uma imagem e sem o antigo limite silencioso de 20 itens de outro comando. Programas de destino precisam aceitar arquivos. Arquivos que desapareceram são reportados; a cópia valida a seleção inteira. Arquivos de outras janelas do LAP são aceitos e deduplicados. Drops de imagens web sem caminho local não são materializados em disco.

## Uso

1. Abra a janela pelo ícone acima de Configurações.
2. Arraste imagens ou pastas, ou use **Adicionar imagens**.
3. Clique para selecionar; ⌘/Ctrl adiciona ou retira da seleção; Shift seleciona um intervalo; ⌘/Ctrl+A seleciona todas.
4. Use **Copiar arquivos**, ⌘/Ctrl+C ou arraste uma imagem selecionada para exportar a seleção original.
5. Espaço ou duplo clique amplia; Escape fecha a prévia. Delete/Backspace e **Retirar da sessão** removem apenas os itens do grid. **Limpar sessão** esvazia a sessão.

## Verificação

- Build de produção do frontend aprovado.
- 42 testes direcionados do frontend aprovados: nova seleção/deduplicação/limite, drop externo, arraste nativo e startup. Corrigido um mock antigo de `nativeDrag.test.mjs` que não fornecia `finishExternalDrag` ao handler avaliado.
- Cinco testes Rust novos aprovados: enumeração, erros, limites, aliases, links simbólicos, miniaturas, rejeição de arquivo corrompido, preservação do diretório original e cópia de mais de 20 caminhos sem truncamento.
- `cargo build --no-default-features` aprovado, produzindo o executável arm64 de desenvolvimento.
- Interface testada no navegador com a ponte Tauri simulada e 20 fotos de demonstração: seleção por Shift, resposta da ação Copiar, preview por Espaço/Escape, retirada de itens e aparência do grid.
- Cópia temporária do aplicativo de desenvolvimento aberta no macOS: botão da barra lateral e janela nativa `/quick-view` observados; também foi observado estado real de sete arquivos locais e seleção. O aplicativo instalado não foi substituído.
- Cópia/colagem efetiva e arraste até um aplicativo de destino externo ainda **não foram verificados ponta a ponta** nesta sessão. Os testes do backend validam os caminhos e o frontend reutiliza o arraste nativo existente.

O diretório de desenvolvimento do LAP já tinha alterações alheias à tarefa em versões, lockfile e configuração. Elas foram preservadas. Não foi criado commit, PR ou instalação de produção.
