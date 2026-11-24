## NAME

gpucheck — vérifier chaque GPU servi par rapport au périphérique logiciel

## SYNOPSIS

`gpucheck`

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
peut pas le faire doit répondre en abandonnant le dessin. Enfin, il
demande au GPU d'exécuter un programme sur plus de groupes de travail
qu'il n'en autorise, ce à quoi tout périphérique doit répondre en
abandonnant ce travail. Un GPU concorde lorsqu'il laisse les mêmes
octets que le périphérique logiciel, refuse ce remplissage pour la même
raison et abandonne le même travail.

La commande affiche une ligne par GPU, qui nomme son nœud matériel :
soit le GPU concorde, soit la première différence trouvée est indiquée
— le travail refusé, et à quelle étape ; un octet qui se lit
autrement ; une réponse différente au remplissage ; une valeur qu'un
programme graphique a laissée différente ; un octet d'une image qui se
lit autrement ; un octet d'un dessin qui se lit autrement ; une réponse
différente au dessin à partir d'une liste ; ou une réponse différente au
travail au-delà des limites du GPU.

`sysinfo gpu` liste les GPU que la commande vérifie.

## OPTIONS

- `-h, -?` — afficher l'aide courte de cette commande.

## EXAMPLES

- `gpucheck` — vérifier chaque GPU servi.

## EXIT STATUS

- `0` — chaque GPU servi a concordé avec le périphérique logiciel.
- `1` — un GPU a différé ou n'a pas pu être joint, aucun GPU n'est
  servi, les GPU servis n'ont pas pu être listés, ou le périphérique
  logiciel n'a pas pu exécuter le travail. Une ligne de commande non comprise
  sort aussi avec `1`.

## ENVIRONMENT

- `LANG` — la locale préférée pour l'aide courte (une étiquette BCP-47
  telle que `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
