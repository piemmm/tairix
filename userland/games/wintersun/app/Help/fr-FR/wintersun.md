## NAME

wintersun — parcourir un monde hivernal généré de façon procédurale

## SYNOPSIS

`wintersun [--reference-scene]`

## DESCRIPTION

Ouvre une fenêtre de bureau sur un monde généré : une vue de dessus d'un
terrain que la machine synthétise au lieu de le livrer, éclairé par un soleil
bas qui étire de longues ombres sur chaque pente.

Rien de ce monde n'est stocké sous forme d'images. Chaque matière dont le sol
est fait — neige, roche, gravier, lande, toundra — tient en quelques nombres
que le client transforme en texture au moment de dessiner, si bien que le
monde est identique sur toutes les machines et n'occupe presque rien sur le
disque. Les routes s'usent dans ce qu'elles traversent au lieu de se poser
dessus.

Le terrain qui n'a pas encore été généré est dessiné comme le vide qu'il est,
puis se remplit à mesure qu'il arrive. Le client dessine ce qu'il a plutôt que
de s'arrêter pour attendre : la fenêtre continue donc de répondre pendant que
le monde rattrape son retard.

Les touches fléchées ou `W`, `A`, `S`, `D` font marcher. Deux touches tenues
ensemble suivent la diagonale à la même vitesse, et deux touches opposées
s'annulent. La vue vous suit et s'arrête au bord du monde au lieu d'en sortir.
Les pentes trop raides à gravir et l'eau trop profonde à traverser vous
écartent.

`+` et `-` rapprochent et éloignent la vue, en cinq paliers, d'une cellule du
monde large de huit pixels à une cellule large de cent vingt-huit.

`F11` passe la fenêtre en plein écran et la rend ensuite à son état précédent :
une fenêtre agrandie revient agrandie. `Échap` la restaure. `Q` quitte.

Chaque détail est dessiné au plus fin jusqu'à ce que vous en décidiez
autrement. La ligne *Settings…* du menu du jeu dans la barre d'icônes ouvre sa
fenêtre de réglages, où la qualité est *Ultra*, chaque détail au plus fin ;
*Basic*, chaque détail au plus simple, à la taille pleine de la fenêtre ;
*Custom*, votre propre choix de l'éclairage, des ombres, de la texture du sol
et de l'échelle de rendu, chacun sur son propre curseur ; ou *Auto*. Déplacer
un curseur rend le choix *Custom*. Votre choix est conservé pour la prochaine
partie.

En *Auto*, le client réduit le détail quand les images arrivent en retard
depuis un moment — l'éclairage d'abord, puis les ombres, puis l'échelle de
rendu — et le rend, palier par palier, à mesure qu'elles se rétablissent. Il
juge sur des secondes plutôt que sur des images isolées, si bien qu'un instant
d'autre travail sur la machine ne coûte rien et qu'une fenêtre plus grande ne
l'envoie pas au plus simple, et il ne dessine jamais les personnages trop
petits pour être lus.

Une fenêtre plus grande que ce que le rendu logiciel peut remplir est dessinée
en 2560×1440 au plus, puis agrandie à la taille de la fenêtre.

## OPTIONS

- `-h, -?, --help` — afficher l'aide courte de cette commande.
- `--reference-scene` — dessiner la scène de référence fixe et la tenir
  immobile : un seul monde, les mêmes personnages et le même instant,
  identiques sur toutes les machines, pour qu'une image de la fenêtre puisse
  être comparée à une autre dessinée ailleurs. `F11` et `Échap` changent
  toujours la taille de la fenêtre ; rien d'autre ne bouge.

## EXIT STATUS

`0` quand vous quittez. Un état non nul indique sa raison sur la sortie
d'erreur : le monde n'a pas pu être généré, la fenêtre n'a pas pu être
ouverte, ou le canal d'événements de la session a été perdu.

- `2` — la ligne de commande n'a pas été comprise.
- `87` — la scène de référence n'a pas pu être dessinée.

## SEE ALSO

`sapper`
