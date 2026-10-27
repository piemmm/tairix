## NAME

blendcheck — comparer la fusion du moteur d'affichage à celle du bureau

## SYNOPSIS

`blendcheck`

## DESCRIPTION

Prend le contrôle de l'écran et y affiche un dégradé de gris sur toute
sa largeur, recouvert de trois vitres sombres, l'une au-dessus de
l'autre :

1. fusionnée par le bureau, avec tramage, comme le bureau dessine les
   fenêtres translucides ;
2. fusionnée par le bureau sans le tramage, ce qui montre les paliers
   que l'effet de bandes laisse dans un dégradé lisse ;
3. fusionnée par le matériel d'affichage lui-même.

Si la vitre du bas ressemble à celle du haut, le matériel d'affichage
fusionne les surfaces translucides sans effet de bandes. Si elle montre
les paliers de celle du milieu, ce n'est pas le cas. La commande
indique aussi si le pilote de l'affichage déclare sa fusion exempte
d'effet de bandes.

Appuyez sur n'importe quelle touche ou sur un bouton du pointeur pour
rendre l'écran.

La commande a besoin de l'affichage pour elle seule : lancez-la donc
depuis une session texte plutôt que depuis un terminal sur le bureau.
Un affichage sans couches matérielles n'a pas de fusion propre à
comparer, et la commande le signale.

## OPTIONS

- `-h, -?` — afficher l'aide courte de cette commande.

## EXAMPLES

- `blendcheck` — afficher la comparaison jusqu'à l'appui sur une touche.

## EXIT STATUS

- `0` — la comparaison a été affichée puis fermée.
- `1` — l'affichage était détenu par une session, n'a pas de couches
  matérielles, a refusé la mire, ou le poste a été perdu pendant
  qu'elle était affichée. Une ligne de commande non comprise sort aussi
  avec `1`.

## ENVIRONMENT

- `LANG` — la locale préférée pour l'aide courte (une étiquette BCP-47
  telle que `fr-FR`).

## SEE ALSO

- `sysinfo`
- `man`
