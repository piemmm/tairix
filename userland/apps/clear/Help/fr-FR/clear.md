## NAME

clear — effacer l'écran du terminal

## SYNOPSIS

`clear [-x]`

## DESCRIPTION

Écrit la séquence qui place le curseur dans le coin supérieur gauche
et efface tout l'affichage, laissant un écran vide. La séquence émise
dépend du terminal nommé par `TERM` ; un terminal incapable d'effacer
(un `TERM` inconnu se replie sur le profil minimal) fait échouer la
commande plutôt que d'imprimer des octets que le terminal afficherait
comme des caractères parasites.

Lorsque le terminal le permet, les lignes qu'il conserve au-dessus de
l'écran — l'historique de défilement — sont également effacées, de sorte
qu'aucun défilement arrière ne permet de revoir ce qui était à l'écran.

## OPTIONS

- `-x` — conserver l'historique de défilement : n'effacer que l'écran.
- `-h, -?` — afficher l'aide courte de cette commande.

## EXAMPLES

- `clear` — effacer l'écran.

## EXIT STATUS

- `0` — la séquence d'effacement a été écrite.
- `1` — le terminal ne peut pas effacer, ou la sortie n'a pas pu être
  délivrée. Une ligne de commande non comprise sort aussi avec `1`.

## ENVIRONMENT

- `TERM` — le terminal dont la séquence d'effacement est écrite.
- `LANG` — la locale préférée pour l'aide courte (une étiquette BCP-47
  telle que `fr-FR`).

## SEE ALSO

- `reset`
- `man`
