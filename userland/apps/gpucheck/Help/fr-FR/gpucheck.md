## NAME

gpucheck — vérifier chaque GPU servi, l'écran qui affiche ce que dessine
l'un d'eux, et votre propre noyau de calcul

## SYNOPSIS

`gpucheck`

`gpucheck --present`

`gpucheck --shader FILE [--words N]`

## DESCRIPTION

Exécute le même court travail sur chaque GPU servi par un pilote et sur
le périphérique logiciel du système, et indique pour chaque GPU si les
deux concordent.

Le travail écrit un motif connu dans une mémoire que la commande peut
lire, remplit la mémoire voisine, copie les deux à travers une mémoire
que le GPU garde pour lui seul puis les recopie en retour, et demande
ensuite un remplissage au-delà de la fin d'un tampon, que tout
périphérique doit refuser. Après cela, le travail exécute chacun des
programmes graphiques propres au système sur une image connue. Si le
GPU peut contenir tous les types d'image qu'utilise l'étape
suivante, le travail copie des images vers la mémoire, depuis
celle-ci et d'une image à l'autre, les efface et exécute un
programme qui lit, écrit et échantillonne des images. Si le GPU peut
dessiner, le travail dessine dans des images des formes, des points et
une ligne de couleurs connues, en masquant ce qui se trouve derrière, en
omettant les formes vues de dos, en fusionnant des couleurs et en
laissant certaines intactes, et réunit quatre échantillons de chaque
pixel en un seul. Il dessine aussi à partir d'une liste en mémoire qui
nomme la première copie d'une forme à dessiner, ce à quoi un GPU qui ne
peut pas le faire doit répondre en abandonnant le dessin. Ensuite, il
demande au GPU d'exécuter un programme sur plus de groupes de travail
qu'il n'en autorise, ce à quoi tout périphérique doit répondre en
abandonnant ce travail. Enfin, il exécute de petits programmes qui,
ensemble, utilisent toutes les instructions permises à un programme GPU,
chaque réponse devant correspondre à celle du périphérique logiciel avec
la précision que garantit la définition de son instruction ; un GPU qui
ne peut pas exécuter l'un d'eux n'y est pas tenu. Un GPU concorde
lorsqu'il laisse les mêmes octets que le périphérique logiciel, refuse
ce remplissage pour la même raison, abandonne le même travail et répond
de la même façon à chacun de ces programmes.

La commande affiche une ligne par GPU, qui nomme son nœud matériel :
soit le GPU concorde, soit la première différence trouvée est indiquée
— le travail refusé, et à quelle étape ; un octet qui se lit
autrement ; une réponse différente au remplissage ; une valeur qu'un
programme graphique a laissée différente ; un octet d'une image qui se
lit autrement ; un octet d'un dessin qui se lit autrement ; une réponse
différente au dessin à partir d'une liste ; une réponse différente au
travail au-delà des limites du GPU ; ou une instruction, dans l'un des
petits programmes, dont la réponse s'écarte de celle du périphérique
logiciel plus que sa définition ne le permet.

`sysinfo gpu` liste les GPU que la commande vérifie.

Avec `--present`, la commande affiche à la place une image connue : un
fond bleu, une bande de couleurs, une rangée de carreaux en damier et un
carreau copié dans le coin opposé, dans une fenêtre qui couvre tout
l'écran. L'image est dessinée par le premier GPU servi par un pilote, ou
par le périphérique logiciel quand aucun n'est servi. La commande la
présente une fois par trame jusqu'à ce qu'on appuie sur Échap ou que la
fenêtre soit fermée, puis indique combien de trames elle a présentées,
ce qui les a dessinées et si l'écran les a toutes affichées.

Avec `--shader`, la commande exécute à la place votre propre programme :
le noyau de calcul contenu dans FILE, écrit en WGSL ou compilé en
SPIR-V, les deux formes se distinguant par le début du fichier. Le WGSL
est compilé dans un processus séparé qui ne peut rien atteindre d'autre,
et chacune des deux formes est vérifiée avant d'être confiée à un GPU.
Un noyau refusé n'est pas exécuté : le refus est affiché, avec, en WGSL,
la ligne et la colonne où il se situe. Le noyau doit avoir un seul point
d'entrée de calcul, utilisant un seul tampon de stockage au groupe 0,
liaison 0. Le tampon contient N mots, 256 sauf indication contraire de
`--words`, le mot *i* valant *i* au départ, et le noyau est exécuté une
fois pour chaque mot. Il s'exécute sur chaque GPU servi par un pilote et
sur le périphérique logiciel ; à côté, dans une mémoire qui appartient à
un programme distinct, le GPU garde un marqueur que le tampon du noyau
ne doit jamais finir par contenir. Pour chaque GPU, la commande affiche
soit que les deux concordent — le tampon se lit de la même façon, ou
tous deux ont abandonné un noyau qui a dépassé son délai et ont continué
à servir —, soit la première différence : un mot qui se lit autrement,
le marqueur trouvé dans le tampon, le noyau refusé, abandonné ou resté
sans réponse là où le périphérique logiciel l'a exécuté, ou un GPU qui
n'a plus rien servi après l'avoir abandonné.

## OPTIONS

- `--present` — afficher l'image en plein écran et vérifier que l'écran
  a affiché chaque trame présentée.
- `--shader FILE` — exécuter le noyau de calcul contenu dans FILE, code
  source WGSL ou module SPIR-V, sur chaque GPU servi et sur le
  périphérique logiciel.
- `--words N` — avec `--shader`, le nombre de mots que contient le
  tampon du noyau : de 1 à 65536, et 256 s'il n'est pas indiqué.
- `-h, -?` — afficher l'aide courte de cette commande.

## EXAMPLES

- `gpucheck` — vérifier chaque GPU servi.
- `gpucheck --present` — afficher l'image en plein écran ; appuyer sur
  Échap pour arrêter.
- `gpucheck --shader double.wgsl` — exécuter sur chaque GPU servi un
  noyau WGSL portant sur 256 mots.
- `gpucheck --shader kernel.spv --words 4096` — exécuter un noyau
  compilé portant sur 4096 mots.

## EXIT STATUS

- `0` — chaque GPU servi a concordé avec le périphérique logiciel ; avec
  `--present`, l'écran a affiché chaque trame présentée ; avec
  `--shader`, chaque GPU servi a concordé avec le périphérique logiciel
  sur le noyau.
- `1` — un GPU a différé ou n'a pas pu être joint, aucun GPU n'est
  servi, les GPU servis n'ont pas pu être listés, ou le périphérique
  logiciel n'a pas pu exécuter le travail ; avec `--present`, une trame
  n'a pas été affichée, aucune n'a été présentée, ou la fenêtre ou le
  périphérique a refusé une étape ; avec `--shader`, le fichier n'a pas
  pu être lu, le noyau a été refusé, ou un GPU a différé. Une ligne de
  commande non comprise sort aussi avec `1`.

## ENVIRONMENT

- `LANG` — la locale préférée pour l'aide courte (une étiquette BCP-47
  telle que `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
