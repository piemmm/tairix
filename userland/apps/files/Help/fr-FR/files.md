## NAME

files — navigateur de fichiers graphique

## SYNOPSIS

`files --desktop [--mount] [chemin] [-h | -?]`

## DESCRIPTION

Le gestionnaire de fichiers du bureau lui-même, qui liste le système de
fichiers dans des fenêtres. Une fenêtre s'ouvre sur le dossier demandé — ou,
pour un fichier, sur le dossier qui contient ce fichier, avec le fichier
sélectionné — ou, à défaut, sur celui que choisissent les paramètres : le
dossier `Home` de l'utilisateur qui le lance, à moins qu'il ne choisisse son
dossier `Desktop` ou les volumes branchés (son répertoire personnel si ce
dossier ne peut être listé). Le titre de la fenêtre nomme le répertoire
courant ; la fenêtre liste ses entrées, chaque entrée sélectionnée mise en
évidence avec la couleur d'accent du thème actif. Chaque lecture de
répertoire est un listage ordinaire, soumis aux permissions, sous l'identité
de l'utilisateur qui le lance : un répertoire illisible est refusé, jamais
deviné.

Le bureau démarre le gestionnaire de fichiers avec la session et le garde
dans la barre d'icônes, et celui-ci ne se termine jamais : le menu de son
emplacement liste vos propres lieux et tout ce qui est monté, et choisir l'un
d'eux y ouvre une fenêtre ; *Réseau* et *Paramètres…* viennent en dernier. Un
clic sur l'emplacement en ouvre une à l'endroit que choisissent les
paramètres. Demander un dossier qui a déjà une fenêtre amène cette fenêtre au
premier plan au lieu d'en ouvrir une autre. Le menu n'a pas de ligne
*Quitter* — le gestionnaire de fichiers fait partie du bureau, fermer ses
fenêtres le range simplement, et si jamais il s'arrête, le bureau le relance.

*Réseau*, dans le menu de l'emplacement juste au-dessus de *Paramètres…*,
ouvre une fenêtre sur les ordinateurs du réseau local qui partagent des
fichiers. Chacun est listé avec son nom, l'adresse à laquelle on le joint,
son groupe de travail ou son domaine, et ce qu'il est, et la liste se
remplit à mesure que les ordinateurs répondent. Le réseau n'est parcouru que
tant qu'une fenêtre l'affiche. Un ordinateur qui ne répond plus est grisé
jusqu'à ce qu'il soit oublié ; un nom qui mélange des alphabets comme
l'exige l'imitation d'un autre nom porte un signe d'avertissement à la place
de son image ; et lorsqu'un ordinateur propose plus que quelques entrées,
les autres sont comptées dans une ligne à part plutôt que d'évincer le
reste. Les noms sur le réseau sont choisis par qui fait fonctionner chaque
ordinateur : un nom ne prouve donc jamais quel ordinateur vous regardez. La
connexion aux partages d'un ordinateur n'est pas disponible sur ce système,
et en ouvrir un le dit.

Il ne s'exécute que lancé par le bureau : lancé de toute autre manière —
depuis un shell par son chemin, `/System/Components/files.app` — il refuse de
démarrer et indique pourquoi sur le flux d'erreur standard. Il exige une
session graphique en cours : sans elle, le canal des fenêtres est
injoignable, et il signale le refus sur le flux d'erreur standard puis se
termine.

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
*Monter en lecture seule*, *Éjecter*, *Lire les informations…*, *Renommer*,
*Couper*, *Copier* et *Supprimer* ; une ligne qui ne peut pas agir là est
grisée et en donne la raison. Viennent ensuite *Nouveau*, puis *Tout
sélectionner* et *Effacer la sélection*, et enfin *Coller*. Un clic droit
sur un espace vide efface la sélection, si bien que le sous-menu de l'entrée
est grisé.

Ouvrir une image disque ou une archive — un fichier `.iso`, `.adf`, `.hdf`,
`.zip` et autres — la monte pour vous sous `/Storage`, en écriture là où le
fichier et son format le permettent tous deux, et ouvre sa fenêtre ;
*Monter en lecture seule* la monte plutôt en lecture seule. En ouvrir une
sur le bureau fait de même. Dès qu'aucune fenêtre ne l'affiche plus, elle
est de nouveau démontée, de sorte que `/Storage` ne contient que ce qui est
en cours d'utilisation ; si autre chose l'utilise encore — par exemple un
document qui en provient, ouvert dans une autre application —, elle reste
montée, et le démontage est retenté dès que cette autre chose a pu la
libérer. Les paramètres peuvent plutôt garder monté ce que vous ouvrez,
jusqu'à ce que vous l'éjectiez ou que votre session de connexion se termine.
*Éjecter* retire un volume que vous avez monté, depuis son icône ou depuis
sa ligne dans la colonne des lieux, dont le menu contextuel propre contient
*Ouvrir* et *Éjecter* ; vos fenêtres qui s'y trouvent passent d'abord à
`/Storage`. Si un volume disparaît — son disque retiré, ou son serveur
perdu — avant que tout ce que vous y avez écrit soit enregistré, il quitte
la colonne des lieux mais garde son icône dans `/Storage`, et ce qui n'a pas
été enregistré est perdu ; *Éjecter* depuis cette icône demande confirmation
avant de le laisser partir.

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
`Ctrl+,`, ouvre les paramètres du navigateur dans l'application Paramètres,
sous *Fichiers* : où s'ouvre une nouvelle fenêtre, comment les fenêtres
affichent et trient leurs entrées, et si une nouvelle montre la colonne des
lieux et la barre d'outils ; si les dossiers restent en tête ; quels suffixes
de nom et quelles entrées sont masqués ; si une image montre son propre
contenu et un dossier ce qu'il contient ; si le déplacement d'entrées vers
la Corbeille demande d'abord confirmation ; et si une image disque ou une
archive que vous avez ouverte est démontée une fois ses fenêtres fermées.
Une modification s'applique aussitôt à toutes les fenêtres et est
mémorisée ; une fenêtre garde un affichage ou un ordre que vous y avez
choisi vous-même.

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

- `--desktop` — s'exécuter comme le gestionnaire de fichiers du bureau
  lui-même : un emplacement permanent sur la barre d'icônes offrant vos lieux
  et les volumes montés, et aucun moyen de quitter. La session de bureau
  passe cette option à chaque lancement du gestionnaire de fichiers, qui ne
  s'exécute qu'avec elle ; un `chemin` donné en plus y ouvre une fenêtre.
- `--mount` — ouvrir `chemin` en tant qu'image disque ou archive, comme
  lorsqu'on l'ouvre dans une fenêtre : elle est montée pour vous et sa
  fenêtre s'ouvre, et elle est de nouveau démontée une fois ses fenêtres
  fermées, selon les paramètres. Cette option exige un `chemin`.
- `-h, -?` — afficher la courte aide de cette commande et quitter.

## EXIT STATUS

Il ne se termine pas tant que le bureau fonctionne. Zéro après l'affichage de
la courte aide ; `2` lorsque la ligne de commande n'a pas été comprise ou ne
nommait pas `--desktop` ; sinon non nul en cas de refus du canal de fenêtre
ou de la région de trames partagée (la raison est indiquée sur le flux
d'erreur standard).
