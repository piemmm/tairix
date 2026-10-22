## NAME

files — navigateur de fichiers graphique

## SYNOPSIS

`files [--desktop] [chemin] [-h | -?]`

## DESCRIPTION

Ouvre une fenêtre de bureau listant le système de fichiers, en partant du
dossier que `chemin` nomme sur la ligne de commande — ou, s'il nomme un
fichier, du dossier qui contient ce fichier, avec le fichier
sélectionné — ou, à défaut, du dossier que choisissent les paramètres : le
dossier `Home` de l'utilisateur qui le lance, à moins qu'il ne choisisse son
dossier `Desktop` ou les volumes branchés (son répertoire personnel si ce
dossier ne peut être listé). Le titre de la fenêtre nomme le répertoire
courant ; la fenêtre liste ses entrées, chaque entrée sélectionnée mise en
évidence avec la couleur d'accent du thème actif. Chaque lecture de
répertoire est un listage ordinaire, soumis aux permissions, sous l'identité
de l'utilisateur qui le lance : un répertoire illisible est refusé, jamais
deviné.

Le bureau démarre le navigateur pour vous et le garde dans la barre
d'icônes : le menu de son emplacement liste vos propres lieux et tout ce qui
est monté, et choisir l'un d'eux y ouvre une fenêtre ; *Paramètres…* vient
en dernier. Un clic sur l'emplacement en ouvre une à l'endroit que
choisissent les paramètres. Demander un dossier qui a déjà une fenêtre amène
cette fenêtre au premier plan au lieu d'en ouvrir une autre. Cette instance
n'a pas de ligne *Quitter* — elle fait partie du bureau, et fermer ses
fenêtres la range simplement.

Lancé par son nom depuis un shell (ou ouvert sur un dossier depuis le
bureau), c'est au contraire une application ordinaire : une seule fenêtre,
et elle se termine quand vous la fermez. Dans tous les cas, il exige une
session graphique en cours : sans elle, le canal des fenêtres est
injoignable, et le navigateur signale le refus sur le flux d'erreur standard
puis se termine.

La fenêtre se pilote au clavier : `Bas` et `Haut` déplacent la sélection,
`Entrée` ouvre le répertoire sélectionné, et `Retour arrière` remonte au
répertoire parent. `F5` relit à la fois le listage et la colonne des lieux ;
un volume nouvellement branché y apparaît de lui-même. `Ctrl+Shift+N` crée
un nouveau dossier.

Un listage s'ouvre sans rien de sélectionné. Un clic sélectionne une entrée,
un clic avec `Ctrl` en ajoute ou en retire une, et un clic avec `Shift`
sélectionne la suite depuis la dernière entrée choisie ; un clic dans un
espace vide efface la sélection. Un double-clic ouvre une entrée ; en
maintenant `Shift`, il liste le contenu d'une application au lieu de la
lancer, et laisse désélectionné tout ce qu'il ouvre ailleurs. Faire glisser
dans un espace vide trace un cadre qui sélectionne tout ce qu'il touche à
mesure qu'il grandit ; maintenu en haut ou en bas du listage, celui-ci
défile, et `Escape` reprend ce que le cadre avait sélectionné.

L'espace autour de chaque icône et de son nom compte comme vide, si bien
qu'un cadre peut commencer n'importe où entre les entrées. `Ctrl+A`
sélectionne tout le listage et `Ctrl+Shift+A` efface la sélection ; les deux
figurent aussi dans le menu contextuel.

Le nom d'une entrée est affiché en entier, sur deux lignes au besoin ; un
nom trop long même pour celles-ci garde son début et sa fin, avec `…` au
milieu, de sorte que son extension reste toujours visible. Une application
est affichée sans son suffixe `.app` ; les paramètres peuvent l'afficher, ou
masquer aussi les suffixes des types de fichiers que le système connaît. Les
entrées dont le nom commence par un point ne sont listées que si les
paramètres le demandent.

`F2` renomme l'entrée sélectionnée sur place, tout comme un clic sur le nom
de la seule entrée sélectionnée suivi d'une pause : le nom s'ouvre à la
modification dès que le clic ne peut plus devenir un double-clic. La partie
qui précède l'extension est sélectionnée, si bien que la saisie remplace le
nom et conserve l'extension ; un suffixe que la fenêtre n'affiche pas est
conservé lui aussi. `Entrée`, ou un clic hors du nom, garde le nouveau nom —
un nom que le volume refuse reste ouvert avec la raison — et `Escape`
l'abandonne. Un clic qui a renommé l'entrée ne fait rien d'autre, car le
dossier a pu être retrié sous le pointeur.

Faire glisser les entrées sélectionnées sur une autre fenêtre du
gestionnaire de fichiers, sur un dossier de celle-ci ou sur le bureau les y
copie ; en maintenant Maj, elles sont déplacées à la place. Le pointeur
montre un plus tant qu'un dépôt copierait et une flèche tant qu'il
déplacerait, et le dossier où un dépôt aboutirait est mis en évidence. Un
fichier seul déposé sur l'emplacement d'une application dans la barre
d'icônes y est ouvert.
Copier ou déplacer conserve les trous de chaque fichier et ses attributs
étendus, tel un type de fichier RISC OS ; tout attribut que la destination
ne peut conserver est nommé dans un avertissement à la fin de l'opération.

Le menu contextuel commence par *Affichage*. En dessous, un sous-menu nomme
ce sur quoi le menu agit — *Fichier « notes.txt »*, un nom long raccourci en
son milieu pour que son extension reste visible, ou *Sélection* quand
plusieurs entrées sont sélectionnées — et contient *Ouvrir*, *Ouvrir avec…*,
*Lire les informations…*, *Renommer*, *Couper*, *Copier* et *Supprimer* ;
une ligne qui ne peut pas agir là est grisée et en donne la raison. Viennent
ensuite *Nouveau*, puis *Tout sélectionner* et *Effacer la sélection*, et
enfin *Coller*. Un clic droit sur un espace vide efface la sélection, si bien
que le sous-menu de l'entrée est grisé.

Le sous-menu *Nouveau* du menu contextuel crée un dossier, ou un document
vide de chaque sorte qu'un éditeur installé sait écrire, et ouvre son nom
pour le modifier.

Le sous-menu *Affichage* du menu contextuel présente les entrées en grandes
icônes, par défaut ; en petites icônes, chaque nom sur une ligne à côté de
son image ; ou en liste, une ligne par entrée avec son image, sa taille, sa
date de dernière modification et son type. Un lecteur qui y figure indique à
la place l'espace qui lui reste, son système de fichiers et l'état de son
service. Sous les modes d'affichage, il trie la fenêtre par nom, type,
taille ou date de dernière modification de chaque entrée — ou, pour une
liste de lecteurs, par nom, espace libre, système de fichiers ou état — dans
l'ordre croissant ou décroissant. Un clic sur le titre d'une colonne trie
aussi la liste selon celle-ci, et un second clic inverse l'ordre. Les
dossiers restent en tête quel que soit l'ordre choisi, à moins que les
paramètres ne les placent parmi les fichiers.

*Paramètres…* dans le menu de l'emplacement de la barre d'icônes, ou
`Ctrl+,`, ouvre les paramètres du navigateur : où s'ouvre une nouvelle
fenêtre, comment elle affiche et trie ses entrées, et si elle montre la
colonne des lieux et la barre d'outils ; si les dossiers restent en tête ;
quels suffixes de nom et quelles entrées sont masqués ; si une image montre
son propre contenu et un dossier ce qu'il contient ; et si le déplacement
d'entrées vers la Corbeille demande d'abord confirmation. *Restaurer les
valeurs par défaut* rétablit chaque paramètre. Une modification s'applique
aussitôt à toutes les fenêtres et est mémorisée ; une fenêtre déjà ouverte
garde l'affichage et l'ordre qu'elle a.

`Alt+Enter` ouvre la fenêtre d'*informations* de l'entrée sélectionnée, tout
comme *Lire les informations…* dans son sous-menu du menu contextuel. C'est une fenêtre à part
entière, si bien que plusieurs peuvent être ouvertes à la fois et que le
listage reste utilisable pendant ce temps : elle montre ce qu'est l'entrée,
sa taille, ses horodatages, la cible d'un alias, ses permissions et son
propriétaire, ainsi que les attributs étendus que le volume conserve pour
elle. Permissions, propriétaire et attributs peuvent y être modifiés, chacun
par une écriture ordinaire, soumise aux permissions, sous votre propre
identité — un refus en donne la raison et ne change rien. Réattribuer un
propriétaire exige la capacité `CAP_FS_CHOWN` ; une session qui ne l'a pas
voit le propriétaire et le groupe marqués d'un cadenas, avec une ligne qui
en donne la raison.

`Gauche` et `Droite` passent d'une section de la fenêtre à l'autre. Dans
*Autorisations*, `Bas` ou `Tab` entre dans ses commandes : les flèches
passent de l'une à l'autre, `Space` bascule une permission ou ouvre le
propriétaire ou le groupe pour le modifier, et `Tab` ou `Escape` revient aux
sections.

L'opérande `chemin` est traité comme une entrée non fiable : ce doit être
un chemin absolu dans la limite de longueur de chemin du système, et
chacun de ses composants doit être un vrai nom — `.` et `..` n'en sont
pas, si bien qu'une écriture ne peut jamais désigner ailleurs que ce
qu'elle donne à lire. Un chemin qui enfreint une de ces règles, ou que
l'utilisateur qui a lancé le programme ne peut pas lister, est refusé
avec la raison sur le flux d'erreur standard et la fenêtre s'ouvre alors
sur le dossier `Home`, de sorte qu'un mauvais argument ne laisse jamais
l'utilisateur sans fenêtre. Un second opérande est refusé d'emblée plutôt
qu'ignoré.

## OPTIONS

- `--desktop` — s'exécuter comme le composant gestionnaire de fichiers du
  bureau lui-même : un emplacement permanent sur la barre d'icônes offrant
  vos lieux et les volumes montés, aucune fenêtre jusqu'à ce qu'on en demande
  une, et aucun moyen de quitter. La session de bureau passe cette option au
  démarrage ; nommer un `chemin` avec elle est refusé, car un composant
  n'ouvre aucune fenêtre où le mettre.
- `-h, -?` — afficher la courte aide de cette commande et quitter.

## EXIT STATUS

Zéro après une fermeture propre, ou après l'affichage de la courte
aide ; `2` lorsque la ligne de commande n'a pas été comprise ; sinon non
nul lorsque le canal de fenêtre, la région de trames partagée ou le
listage initial du répertoire a été refusé (la raison est indiquée sur
le flux d'erreur standard).
