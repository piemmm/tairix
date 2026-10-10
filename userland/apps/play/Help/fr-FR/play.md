## NAME

play — jouer des fichiers sonores

## SYNOPSIS

`play [option...] file...`

## DESCRIPTION

Joue chaque fichier à son tour sur une sortie. Chaque fichier est décodé dans
un processus isolé qui ne détient aucune autorité : un fichier hostile ne peut
au pire que mettre fin à son propre décodage. Il est alors écarté avec la
raison indiquée, et le reste de la liste est joué. Les fichiers AU, WAV et FLAC
sont lus, FLAC natif ou dans Ogg.

Les fichiers consécutifs de même fréquence, même format d'échantillon et même
disposition des canaux s'enchaînent sans blanc, dans un seul flux. Un fichier
d'une autre forme attend que ce qui est en file soit joué, puis ouvre son
propre flux.

Lorsque l'entrée standard est un terminal, `play` dessine une interface plein
écran : le fichier joué, la position, le niveau et un indicateur par canal, et
la liste. La lecture n'en dépend pas. Mis en arrière-plan, `play` continue de
jouer et rend le terminal ; ramené au premier plan, il se redessine. Sans
interface, il affiche une ligne de progression sur la sortie d'erreur d'un
terminal.

L'interface accepte ces touches : Espace ou `p` met en pause et reprend ; les
flèches gauche et droite avancent ou reculent de dix secondes ; `n` ou `>`
passe au fichier suivant ; `b` ou `<` revient au début de ce fichier, ou au
précédent durant ses trois premières secondes ; `+`, `=` ou la flèche haut
monte de trois décibels, `-`, `_` ou la flèche bas baisse d'autant ; `q` ou
Ctrl-C arrête ; Ctrl-Z suspend, après avoir mis le flux en pause.

Le niveau d'un flux est une atténuation : un flux ne peut dépasser la pleine
échelle, donc `--gain` et les touches de niveau ne vont pas au-delà de 0 dB.
Pour jouer plus fort, montez le volume de la sortie.

Un temps s'écrit `[[HH:]MM:]SS[.fraction]` — `90`, `1:30`, `1:02:03` ou `12.5`.

Sur l'information standard (fd 3), `play` écrit un enregistrement `schema`
pour chaque fichier joué, un enregistrement `omission` pour chaque fichier
écarté ou interrompu, et un enregistrement `summary` à la fin.

## OPTIONS

- `-q, --quiet` — ni interface ni ligne de progression.
- `-v, --verbose` — le format et la durée de chaque fichier sur la sortie d'erreur.
- `--ui, --no-ui` — dessiner l'interface, ou jamais ; `--ui` sans terminal est
  refusé.
- `-d, --device <sink>` — la sortie : `audio:sink/default`,
  `audio:sink/<id>` pour ce démarrage, ou `audio:sink/<location>` où que
  soit le périphérique, tels que `--list-devices` les nomme.
- `-g, --gain <dB>` — le niveau du flux, 0 ou moins, au centième.
- `-s, --start <time>` — commencer chaque fichier à ce temps.
- `-t, --duration <time>` — jouer cette durée de chaque fichier.
- `-l, --loop[=N]` — jouer la liste N fois en tout, ou sans N indéfiniment.
- `--list-devices` — nommer les sorties accessibles à la session, chacune
  par son identifiant et son emplacement, puis quitter.
- `-h, -?, --help` — afficher l'aide courte de cette commande.
- `--version` — afficher la version, puis quitter.

## EXAMPLES

- `play song.wav` — jouer un fichier, avec l'interface sur un terminal.
- `play -q intro.au song.wav &` — jouer une liste en arrière-plan.
- `play -s 1:30 -t 20 song.wav` — jouer vingt secondes à partir d'une minute trente.
- `play -l3 -g -6 loop.wav` — jouer un fichier trois fois, six décibels plus bas.
- `play --list-devices` — voir les sorties.

## EXIT STATUS

- `0` — chaque fichier a été joué, ou la lecture a été arrêtée sans fichier écarté.
- `1` — un fichier n'a pas pu être joué, ou la lecture n'a pas pu continuer.
- `2` — la ligne de commande n'a pas été comprise.

## ENVIRONMENT

- `TERM` — le terminal pour lequel l'interface dessine.
- `LANG` — la langue préférée de l'aide courte (une étiquette BCP-47 comme
  `fr-FR`).

## SEE ALSO

- `man`
