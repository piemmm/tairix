## NAME

terminal — émulateur de terminal graphique

## SYNOPSIS

`terminal`

## DESCRIPTION

Ouvre une fenêtre de bureau hébergeant le shell par défaut de
l'utilisateur sur un écran de 80×25 caractères. Les touches tapées dans
la fenêtre active sont envoyées au shell ; tout ce que le shell écrit
(sortie standard comme erreur standard) est interprété via le
vocabulaire ANSI/VT partagé et dessiné avec la palette de couleurs
choisie dans les paramètres. Le terminal lui-même ne fait jamais d'écho :
l'écho et l'édition de ligne appartiennent au shell, exactement comme
sur une console.

La fenêtre s'ouvre aux dimensions que mesure l'écran 80×25 dans la
taille de texte en vigueur, afin qu'elle s'adapte à l'affichage sur
lequel elle est présentée ; sur un écran trop petit pour cette taille,
le texte est réduit plutôt que la fenêtre rétrécie, car un programme qui
se conçoit pour 80 colonnes doit toujours les obtenir.

Le terminal se lance depuis la Bibliothèque de programmes du bureau (le
bouton `Library` de la barre des tâches) ou par son nom depuis un
shell. Il requiert une session graphique active : sans elle, le canal de
fenêtre est inaccessible et le terminal signale le refus sur le flux
d'erreur standard puis se termine.

La session se termine quand le shell quitte (par exemple avec `exit`)
ou quand la fenêtre est fermée depuis le bureau ; fermer la fenêtre
termine le shell par une fin de fichier sur son entrée.

Fermer la dernière fenêtre ne termine pas le terminal. Son icône reste
dans la barre d'icônes du bureau, et un clic dessus ouvre une nouvelle
fenêtre ; *Quitter* dans le menu de cette icône ferme toutes les
fenêtres et termine le programme.

Les lignes qui sortent par le haut de l'écran sont conservées —
les 500 dernières, sauf réglage contraire dans *Paramètres…* — et la
molette de la souris permet d'y remonter ; `Shift Page Up`
et `Shift Page Down` font défiler d'un écran à la fois, `Shift Home` va
à la ligne la plus ancienne et `Shift End` revient à l'écran. La sortie
qui arrive pendant que vous êtes remonté dans l'historique laisse la vue
où elle est, et la saisie la ramène à l'écran. Redimensionner la fenêtre
recompose les lignes à la nouvelle largeur au lieu de les tronquer, et
l'écran propre d'un programme plein écran n'en conserve aucune. Quand la
mémoire vient à manquer, les lignes les plus anciennes sont libérées en
premier.

Appuyer sur le bouton secondaire (droit) de la souris n'importe où sur
l'écran ouvre le menu du terminal. Chaque ligne, sauf
*Effacer l'historique*, dispose d'un raccourci clavier qui fonctionne
que le menu soit ouvert ou non, et `Escape` — ou un clic en dehors du
menu — le ferme sans rien choisir.

| Ligne | Raccourci | Ce qu'elle fait |
| --- | --- | --- |
| Paramètres… | `Ctrl ,` | Ouvre les paramètres décrits ci-dessous. |
| Texte plus grand | `Ctrl +` | Dessine l'écran un pas plus grand. |
| Texte plus petit | `Ctrl -` | Dessine l'écran un pas plus petit. |
| Taille réelle | `Ctrl 0` | Revient à la taille de texte par défaut. |
| Effacer l'écran | `Ctrl Shift K` | Vide l'écran sans écrire dans le shell ; ce qu'il affichait reste accessible en remontant. |
| Effacer l'historique | — | Oublie les lignes conservées au-dessus de l'écran. |
| Fermer | `Ctrl Shift W` | Ferme la fenêtre et termine le shell. |

Les paramètres s'ouvrent dans une fenêtre à part, qui peut rester
ouverte pendant que vous travaillez dans le terminal ; choisir de
nouveau *Paramètres…* la ramène au premier plan. Elle comporte deux
onglets ; tournez la molette au-dessus d'eux pour faire défiler.
*Terminé*, `Escape` ou le bouton de fermeture de la fenêtre la ferme.
**Apparence** choisit la palette de couleurs, définit la taille du texte
et le nombre de lignes conservées dans l'historique (d'aucune à 10 000,
500 au départ), et modifie la palette propre de l'utilisateur. Les palettes
fournies sont *System* (qui suit l'apparence sombre ou claire du
bureau), *Midnight*, *Phosphor*, *Amber*, *Ember*, *Contrast*, *Paper*
et *Custom*. Choisir *Custom* utilise les couleurs modifiées sous le
sélecteur : une grille des vingt couleurs dont un écran est composé — le
fond, le premier plan, le curseur, le texte du curseur et les seize
couleurs ANSI — et un sélecteur de couleur pour celle qui est
sélectionnée. Le sélecteur la règle par teinte, saturation et valeur, par
rouge, vert et bleu, ou par sa notation hexadécimale, et montre à côté la
couleur qu'avait la case au moment de la sélection, qu'un clic rétablit.

**Effets** définit la manière dont l'écran est dessiné. Un nouveau
terminal s'ouvre avec 80 % d'opacité, le flou d'arrière-plan à
mi-intensité et tous les autres effets désactivés, de sorte que le bureau
apparaît derrière le texte comme à travers un verre dépoli au lieu de
transparaître nettement.

| Effet | Ce qu'il fait |
| --- | --- |
| Opacité | La solidité de l'arrière-plan. En dessous du plein, le bureau transparaît derrière le texte, qui reste parfaitement lisible. |
| Flou d'arrière-plan | Le degré de flou du bureau derrière une fenêtre transparente. N'a aucun effet sur une fenêtre totalement opaque. |
| Lignes de balayage | Assombrit une ligne sur deux, l'aspect plat d'un masque d'ombre. |
| Halo | Diffuse la lumière des pixels lumineux dans leur voisinage, de sorte que le texte porte le halo doux d'un tube poussé à fond. |
| Bruit | Un bruit de fond par pixel en mouvement, comme celui d'un signal analogique. |
| Phosphore | La durée de persistance des pixels allumés, de sorte que le texte qui défile rapidement laisse une traînée. |
| Ondulation | Un lent vacillement horizontal mouvant, comme celui d'un tube déréglé. |

Chaque modification prend effet immédiatement et est enregistrée dans le
profil propre de l'utilisateur, de sorte qu'un terminal ultérieur s'ouvre
de la même manière. Le système d'exploitation conserve le profil via son
service de réglages, et celui-ci est privé au terminal : aucune autre
application ne peut le lire ni le modifier. Seul ce que l'utilisateur a
réellement changé est enregistré ; *Restaurer les valeurs par défaut*
supprime donc ces choix au lieu de figer les valeurs du jour — ce que
l'administrateur ou une version ultérieure du terminal modifie s'applique
alors. Un réglage que le terminal ne sait pas interpréter reste à sa valeur
par défaut et est signalé sur le flux d'erreur standard, et un service de
réglages inaccessible laisse le terminal fonctionner avec les valeurs
livrées, ce qui est également signalé.

## EXIT STATUS

Zéro après une fermeture propre ou la sortie du shell ; non nul quand
le shell n'a pas pu être hébergé ou quand le canal de fenêtre, la
région de trame partagée ou la boîte d'événements a été refusé (la
raison est indiquée sur le flux d'erreur standard).

## ENVIRONMENT

`HOME`
: Le répertoire personnel du compte, où le terminal lit et écrit son
profil. Sans lui, le terminal fonctionne sur le profil par défaut et
n'enregistre rien.

`TERM`
: Exporté vers le shell hébergé sous la valeur `xterm-256color`, qui
nomme l'émulateur que ce terminal présente. Toute valeur héritée est
remplacée ; le reste de l'environnement est transmis au shell tel quel.

## SEE ALSO

`elsh`, `sysinfo`
