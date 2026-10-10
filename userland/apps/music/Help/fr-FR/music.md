## NAME

music — le lecteur de musique du bureau

## SYNOPSIS

`music`

## DESCRIPTION

Joue une liste de pistes dans une fenêtre. Ouvrez des fichiers ou un dossier
entier avec le sélecteur de fichiers du bureau, ou ouvrez une piste depuis le
gestionnaire de fichiers : elle rejoint la liste du lecteur déjà ouvert. Les
pistes de même fréquence, même format d'échantillon et même disposition de
canaux s'enchaînent sans blanc.

Le lecteur ne détient aucune capacité sur le système de fichiers. La session
du bureau parcourt les fichiers pour lui et lui délègue, une seule fois et en
lecture seule, exactement les fichiers que l'utilisateur choisit — pour un
dossier, les fichiers qu'il contient et que ce lecteur ouvre. Aucun fichier
n'est décodé dans le lecteur : le son et la pochette sont chacun décodés par un
processus séparé qui n'a aucune portée, si bien qu'un fichier malformé ou
hostile ne peut rien atteindre de ce que le lecteur peut atteindre.

Le haut de la fenêtre montre ce qui joue : la pochette, le titre, l'artiste et
l'album, le format, la position dans la piste et un indicateur de niveau par
canal. Dessous se trouvent le transport, les commandes de lecture aléatoire et
de répétition et le volume, puis la liste. Faites glisser le curseur de
position pour vous déplacer et celui du volume pour régler le niveau ; tous
deux agissent là où vous relâchez. Double-cliquez une piste pour la jouer. Un
appui secondaire sur la liste ouvre le menu du lecteur, qui choisit aussi la
sortie et le nivellement des pistes selon l'intensité indiquée par leurs
propres étiquettes.

Un fichier que le lecteur ne peut pas lire est écarté, avec sa raison sur la
ligne d'état.

* `Space` — lire ou mettre en pause
* `Enter` — jouer la piste sélectionnée
* `Left` / `Right` — dix secondes en arrière ou en avant
* `Ctrl` + `Left` / `Right` — la piste précédente ou la suivante
* `Up` / `Down` — sélectionner la piste au-dessus ou au-dessous
* `Alt` + `Up` / `Down` — déplacer la piste sélectionnée
* `Delete` — retirer la piste sélectionnée de la liste
* `+` / `-` — trois décibels plus fort ou plus doux
* `S` — lecture aléatoire ou dans l'ordre
* `R` — ne rien répéter, répéter la liste ou la piste
* `Ctrl` + `O` — ouvrir des fichiers
* `Ctrl` + `Shift` + `O` — ouvrir un dossier

## OPTIONS

`-h`, `-?`, `--help`
: Écrire cette aide sur la sortie standard et quitter.

## EXIT STATUS

Zéro une fois la fenêtre fermée ou Quitter choisi. Non nul lorsque le canal de
fenêtre, la zone d'image partagée ou la session du bureau a été refusé ; la
raison est donnée sur l'erreur standard.
