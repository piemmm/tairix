## NAME

blendcheck — comparar a mistura do motor do ecrã com a do ambiente de trabalho

## SYNOPSIS

`blendcheck`

## DESCRIPTION

Assume o controlo do ecrã e mostra uma rampa de cinzentos a toda a sua
largura, à qual sobrepõe três painéis de vidro escuro, dispostos uns
acima dos outros:

1. misturado pelo ambiente de trabalho, com pontilhado, tal como o
   ambiente de trabalho desenha as janelas translúcidas;
2. misturado pelo ambiente de trabalho sem o pontilhado, o que mostra os
   degraus que o efeito de bandas deixa num gradiente suave;
3. misturado pelo próprio hardware do ecrã.

Se o painel de baixo parecer igual ao de cima, o hardware do ecrã faz a
mistura translúcida sem efeito de bandas. Se mostrar os degraus do
painel do meio, não o faz. O comando também imprime se o controlador do
ecrã declara que a sua mistura é isenta de bandas.

Prima qualquer tecla ou botão do ponteiro para devolver o ecrã.

O comando precisa do ecrã só para si, por isso execute-o a partir de
uma sessão de texto e não de um terminal no ambiente de trabalho. Um
ecrã sem camadas de hardware não tem mistura própria para comparar, e o
comando indica-o.

## OPTIONS

- `-h, -?` — mostrar a ajuda curta deste próprio comando.

## EXAMPLES

- `blendcheck` — mostrar a comparação até se premir uma tecla.

## EXIT STATUS

- `0` — a comparação foi mostrada e fechada.
- `1` — o ecrã estava ocupado por uma sessão, não tem camadas de
  hardware, recusou o padrão, ou o posto perdeu-se durante a comparação.
  Uma linha de comandos não compreendida também termina com `1`.

## ENVIRONMENT

- `LANG` — a região preferida para a ajuda curta (uma etiqueta BCP-47
  como `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
