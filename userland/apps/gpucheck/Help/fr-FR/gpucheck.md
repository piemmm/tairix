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
enfin un remplissage au-delà de la fin d'un tampon, que tout
périphérique doit refuser. Un GPU concorde lorsqu'il laisse les mêmes
octets que le périphérique logiciel et refuse ce remplissage pour la
même raison.

La commande affiche une ligne par GPU, qui nomme son nœud matériel :
soit le GPU concorde, soit la première différence trouvée est indiquée
— un octet qui se lit autrement, le travail refusé, ou une réponse
différente au remplissage.

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
