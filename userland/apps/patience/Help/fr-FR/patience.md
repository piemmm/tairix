## NAME

patience — Klondike, FreeCell et Spider, avec un solveur derrière chaque indice

## SYNOPSIS

`patience`

## DESCRIPTION

Ouvre une fenêtre du bureau disposée pour une partie de patience, ces jeux de
cartes que l'on appelle aussi réussites ou solitaires. Six jeux figurent dans le
menu Game, et les touches `1` à `6` les choisissent : le Klondike en tirant une
carte ou trois, le FreeCell, et le Spider à une, deux ou quatre familles.

Au Klondike, montez les sept colonnes en descendant et en alternant les
couleurs, et envoyez chaque famille sur sa fondation, de l'as au roi. Cliquez
sur la pioche pour retourner des cartes sur le talon ; une fois la pioche vide,
cliquez sur son emplacement pour retourner le talon. Au FreeCell, toutes les
cartes sont distribuées face visible et chacune des quatre cases libres reçoit
une carte ; une suite se déplace d'un bloc par les cases libres et les colonnes
vides tant qu'elles ont la place de la porter. Au Spider, montez en descendant
dans n'importe quelle famille, mais ne déplacez une suite que si elle est d'une
seule famille ; une suite complète du roi à l'as rejoint les fondations
d'elle-même. Cliquez sur la pioche pour distribuer une carte sur chaque colonne,
ce qui exige d'abord que chaque colonne en porte une.

Faites glisser les cartes où vous voulez, ou cliquez dessus : un clic envoie les
cartes au meilleur endroit possible, d'abord sur les fondations, puis sur une
autre colonne, puis dans une colonne vide, puis dans une case libre. Les cartes
qui ne peuvent aller nulle part sautillent sur place. Les cartes dont aucun coup
ultérieur ne pourrait avoir besoin rejoignent les fondations d'elles-mêmes, et
dès que plus rien ne peut empêcher la partie d'être gagnée, toutes le font.
Lorsque Send cards home est désactivé, Finish (`F`) les y envoie.

Chaque indice vient d'un solveur qui joue la partie en avance sur vous : un
indice (`H`) est donc un coup qui garde la partie gagnable, et pas seulement un
coup que les règles permettent. Le solveur examine chaque position que vous
atteignez. Lorsqu'il prouve qu'il ne reste aucune ligne gagnante, la barre
d'état l'indique, et Back to last winnable (`Ctrl+B`) vous ramène à la dernière
position depuis laquelle la partie pouvait encore être gagnée.

Lorsque Winnable deals only est activé, comme au départ, chaque nouvelle donne
est une donne que le solveur a déjà gagnée, ce qu'indique une coche à côté de
son numéro. Le solveur renonce à une donne qu'il ne parvient pas à trancher
vite, et la donne qu'il vous propose alors est marquée comme non vérifiée ; le
Spider à quatre familles est souvent trop profond pour être tranché à temps.
Chaque donne est numérotée dans la barre d'état, et le menu propre à la table,
ouvert avec le bouton secondaire, joue une donne d'après son numéro. Les donnes
du FreeCell sont numérotées comme les autres jeux de FreeCell les numérotent :
une donne numérotée est donc la même partie partout.

Le Klondike se compte de façon standard, ou à la manière de Las Vegas, où chaque
partie coûte 52 $ et chaque carte envoyée sur les fondations rapporte 5 $, avec
un seul passage dans la pioche en tirant une carte et trois en tirant trois, ou
à la manière de Las Vegas cumulée, qui reporte les gains d'une partie à l'autre.
Le Spider part de 500 points, en perd un à chaque coup et en gagne 100 pour
chaque suite envoyée sur les fondations. Pour chaque jeu, les statistiques
conservent les parties jouées et gagnées, la série en cours de victoires ou de
défaites, la plus longue série de victoires, la victoire la plus rapide et la
victoire en le moins de coups. Redistribuer avant qu'une partie soit gagnée la
compte comme perdue.

La partie en cours est conservée : fermer la fenêtre ou quitter la reporte à la
fois suivante, et l'horloge s'arrête tant que la fenêtre est masquée. Le menu du
jeu dans la barre d'icônes et le menu propre à la table réunissent les options :
les donnes gagnables, le mode de comptage, l'envoi des cartes sur les
fondations, les couleurs à quatre teintes, le dos des cartes et la table.

Le clavier permet tout ce que permet le pointeur. Les touches fléchées déplacent
un anneau de pile en pile et le long des cartes d'une colonne ; `Space` prend
les cartes et les pose là où se trouve l'anneau, `Enter` les envoie au meilleur
endroit et `Escape` les remet en place. `D` retourne la pioche, `Ctrl+Z` annule
un coup et `Ctrl+Y` le rejoue, `N` distribue une nouvelle partie, `R`
redistribue celle-ci et `S` affiche les statistiques.

Le jeu se lance depuis la bibliothèque de programmes du bureau, sous Games, ou
par son nom depuis un interpréteur de commandes. Il exige une session graphique
en cours : sans elle le canal de fenêtre est inaccessible et le jeu signale le
refus sur le flux d'erreur standard puis se termine.

## EXIT STATUS

Zéro après une sortie propre ; non nul lorsque le canal de fenêtre ou la région
de trame partagée a été refusé (la raison est indiquée sur le flux d'erreur
standard).
