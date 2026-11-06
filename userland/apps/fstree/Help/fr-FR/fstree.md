## NAME

fstree — le gestionnaire de fichiers arborescent plein écran

## SYNOPSIS

`fstree [directory]`

## DESCRIPTION

Parcourt le système de fichiers dans une session plein écran pilotée au
clavier, sur le modèle de XTree Gold : un en-tête de statistiques disque
en haut, une fenêtre d'arborescence de répertoires au-dessus d'une
fenêtre de fichiers qui liste les entrées du répertoire mis en évidence
avec leurs tailles et leurs dates de modification. La session démarre
dans `directory` (la vue racine `/` par défaut).

La fenêtre d'arborescence est la principale : la barre de sélection s'y
déplace et la fenêtre de fichiers montre toujours le répertoire mis en
évidence. `Enter` (ou `Tab`) passe dans la fenêtre de fichiers pour agir
sur les fichiers ; `Esc` (ou `Tab`) revient à l'arborescence — la
navigation commence toujours dans l'arborescence.

L'arborescence est lue paresseusement : le contenu d'un répertoire n'est
récupéré que lorsqu'il est affiché ou déplié pour la première fois, si
bien que parcourir un très grand volume ne coûte que les répertoires
réellement ouverts. Un répertoire que l'appelant ne peut pas lister est
refusé sur place — l'erreur apparaît sur la ligne de message et la vue
précédente est conservée ; rien n'est fabriqué.

Touches :

- `Up`/`Down` ou `k`/`j` — déplacer la sélection de la fenêtre active ;
  `PageUp`/`PageDown` la déplacent d'un écran, `Home`/`End` sautent à la
  première ou à la dernière ligne. Déplacer la sélection de
  l'arborescence liste le répertoire nouvellement mis en évidence dans la
  fenêtre de fichiers.
- `Right`/`l` — dans l'arborescence, déplier la branche mise en évidence
  (lue paresseusement) ; dans la fenêtre de fichiers, descendre dans le
  répertoire mis en évidence.
- `Left`/`h` — dans l'arborescence, replier la branche mise en évidence
  ou — quand elle est déjà repliée ou sans sous-répertoires — remonter la
  sélection au répertoire parent ; dans la fenêtre de fichiers, revenir à
  l'arborescence.
- `+`/`-` — déplier / replier la branche mise en évidence de
  l'arborescence.
- `Enter` — dans l'arborescence, passer dans la fenêtre de fichiers ;
  dans la fenêtre de fichiers, descendre dans le répertoire mis en
  évidence (les deux fenêtres suivent), ou ouvrir le fichier mis en
  évidence dans une visionneuse plein écran : la visionneuse de
  désassemblage pour un exécutable reconnu, la visionneuse de texte quand
  les premiers octets du fichier se lisent comme du texte, le vidage
  hexadécimal sinon (les visionneuses sont décrites plus bas).
- `Esc` — annuler un calcul d'usage disque en cours (en conservant ses
  chiffres) ; sinon revenir de la fenêtre de fichiers à la fenêtre
  d'arborescence.
- `o` — ouvrir le fichier mis en évidence dans la visionneuse de votre
  choix : `t` texte, `x` hexadécimal ou `d` désassemblage.
- `Tab` — basculer entre la fenêtre d'arborescence et la fenêtre de
  fichiers.
- `s` — ouvrir le menu de tri : `n` nom, `e` extension, `s` taille,
  `m` date de modification, `r` inverser le sens, `Esc` annule. Les
  répertoires sont toujours groupés avant les fichiers.
- `c` — copier l'entrée sélectionnée : une invite demande la
  destination. Une destination relative atterrit dans le répertoire
  listé ; une destination qui est un répertoire existant reçoit la copie
  à l'intérieur, sous le nom de la source. `Tab` complète le chemin
  saisi — une correspondance unique est insérée (un répertoire restant
  ouvert avec son `/`), plusieurs correspondances sont prolongées jusqu'à
  leur préfixe commun ou listées. Un répertoire est copié avec tout ce
  qu'il contient. Copier une entrée sur elle-même ou un répertoire dans
  son propre sous-arbre est refusé avant toute écriture.
- `m` — déplacer l'entrée sélectionnée, avec la même invite de
  destination. Au sein d'un même volume, le déplacement est un renommage
  atomique ; entre volumes, l'entrée est copiée puis la source
  supprimée.
- `r` — renommer l'entrée sélectionnée sur place : l'invite est
  pré-remplie avec le nom actuel.
- `d` — supprimer l'entrée sélectionnée après confirmation ; seul `y`
  procède. Supprimer un répertoire retire tout ce qu'il contient, et la
  confirmation le dit. La confirmation peut être désactivée dans le menu
  des réglages (`S`).
- `M` — créer un répertoire dans le répertoire listé ; son nom est
  demandé.
- `a` — ouvrir les attributs de l'entrée sélectionnée : ses bits de
  permission et ses attributs étendus (paires `namespace.key = value`,
  par exemple `user.comment`). Dans la vue, les flèches parcourent les
  attributs, `m` modifie le mode (une invite octale pré-remplie avec les
  bits actuels ; Entrée applique — seul le propriétaire de l'entrée peut
  le modifier, le noyau refuse quiconque d'autre), `n` ajoute un
  attribut sous la forme d'une ligne `key=value`, Entrée modifie
  l'attribut sélectionné (pré-rempli ; pour une valeur binaire, seule la
  clé est pré-remplie), `d` le supprime, et Échap quitte la vue. Le noyau
  décide de chaque modification — la lecture et l'écriture suivent les
  permissions propres de l'entrée, les espaces de noms
  `system.`/`trusted.` sont réservés, et un refus est affiché tel quel.
  Un système de fichiers dont le format ne stocke pas d'attributs
  (FAT32, ext4) le signale dans la vue ; l'éditeur de mode y fonctionne
  quand même.
- `t` — marquer ou démarquer l'entrée sélectionnée du panneau de
  fichiers et descendre d'une ligne ; des pressions répétées marquent
  donc une série. Les entrées marquées portent un `*`.
- `T` — marquer par motif : un glob (`*`, `?`, `[...]`) comparé aux
  noms visibles, ou une plage — `size:MIN..MAX` (octets, suffixes
  `K`/`M`/`G`/`T`, chaque borne pouvant rester ouverte : `size:1M..`,
  `size:..64K`) ou `date:YYYY-MM-DD..YYYY-MM-DD` (date de modification,
  date de fin incluse ; une entrée dont le stockage ne conserve aucune
  date ne correspond jamais à une date). Chaque correspondance rejoint
  l'ensemble marqué ; un motif ou une plage mal formés ne marquent rien
  et disent pourquoi.
- `i` — inverser les marques sur les entrées visibles.
- `C` — effacer toutes les marques.
- `f` — filtrer le panneau de fichiers par un glob de nom de fichier,
  appliqué en direct pendant la saisie. `Enter` conserve le filtre
  (affiché dans la ligne d'état), `Esc` rétablit le filtre tel qu'il
  était. Un motif qui ne compile pas ne masque rien — la ligne d'état le
  signale plutôt par `(bad pattern)`. Vider le motif supprime le filtre.
- `/` — chercher dans la branche sous le répertoire ciblé par glob de
  nom de fichier, comparé au chemin de chaque fichier relatif à la
  branche. Les résultats arrivent dans la vue aplatie au fur et à mesure
  qu'ils sont trouvés.
- `F` — chercher un texte littéral dans le contenu des fichiers, sans
  tenir compte de la casse et lu par fenêtres bornées (une
  correspondance qui chevauche une limite de lecture est quand même
  trouvée). Si des entrées sont marquées, la recherche couvre l'ensemble
  marqué (les répertoires marqués récursivement) ; sinon elle couvre la
  branche ciblée. Chaque ligne de résultat porte son nombre de
  correspondances ; un fichier dont le contenu semble binaire est
  signalé comme correspondance binaire — ses octets ne sont jamais
  affichés. Un fichier illisible figure dans le rapport du
  parcours, jamais omis en silence.
- `u` — compter l'usage disque sous le répertoire ciblé : fichiers,
  octets et répertoires, parcourus progressivement en arrière-plan.
  `Esc` annule en conservant les chiffres déjà comptés.
- `v` — aplatir la branche sous le répertoire ciblé : une liste de
  chaque fichier en dessous, remplie page par page (`Space` charge la
  page suivante). Dans la vue, `t`/`T`/`i`/`C` marquent ses lignes,
  `c`/`m`/`d` lancent des opérations groupées sur l'ensemble marqué,
  `Enter` saute au répertoire de la ligne sélectionnée dans les panneaux
  (le curseur se posant sur son entrée), et `Esc` revient aux panneaux.
  Tant qu'un parcours ou une recherche est en cours, `Esc` l'arrête
  d'abord en conservant les lignes déjà trouvées. Les lignes sont nommées
  relativement à la branche aplatie. Les résultats de recherche (`/`,
  `F`) remplissent cette même vue, si bien que leurs lignes se marquent
  et se traitent exactement comme une liste aplatie.
- `H` — afficher/masquer les entrées cachées (noms à point) dans les deux
  panneaux.
- `.` — répéter la dernière opération sur fichier avec la sélection
  courante : une copie ou un déplacement va de nouveau dans le même
  répertoire de destination, une suppression redemande selon le réglage
  de confirmation. Sans opération préalable, la touche le dit.
- `V` — lister les volumes montés : chaque ligne montre le point de
  montage, le type de système de fichiers et les octets libres/totaux
  quand le volume les rapporte. `Enter` réenracine toute la session sur
  le volume choisi ; `Esc` ferme la liste. Quand aucun volume n'est
  rapporté, la touche le dit.
- `S` — le menu des réglages : `1` active ou désactive la confirmation
  de suppression unitaire, `2` celle de la suppression groupée. Les
  modifications sont enregistrées dans les réglages propres de cette
  application, qu'elle seule peut lire, et chargées au démarrage
  suivant ; quand le service de réglages est inaccessible, elles ne
  valent que pour la session et le menu dit pourquoi.
- `?` — afficher cette aide par-dessus les panneaux ; toute touche la
  ferme.
- `q` — quitter en restaurant le terminal.

Tant que des entrées sont marquées, `c`, `m` et `d` agissent sur
l'ensemble marqué plutôt que sur la sélection : `c`/`m` demandent un
répertoire de destination existant où atterrissent les entrées (`Tab` le
complète), et `d` confirme la suppression groupée (sauf si cette
confirmation est désactivée dans le menu des réglages). Les entrées sont
traitées dans l'ordre de marquage ; un échec n'arrête jamais le reste, le
rapport final compte ce qui a réussi et un écran de rapport nomme chaque
échec — un lot n'est jamais silencieusement partiel. Les entrées réussies
sont démarquées ; les échecs restent marqués pour un nouvel essai.

Quand une copie ou un déplacement écraserait un fichier existant, la
session demande fichier par fichier : `o` écrase, `s` saute (une source
sautée reste en place), et `c` annule les étapes restantes — dans un
lot, l'annulation abandonne toutes les entrées restantes — ce qui a
déjà été appliqué le reste, et le rapport final dit ce qui s'est
passé. Un échec en cours de copie supprime la cible à moitié écrite et
affiche l'erreur du noyau ; rien ne se fait jamais passer pour une
copie complète. Chaque opération est autorisée par le noyau — un refus
apparaît tel quel sur la ligne de message, sans que rien ne change.
Une copie conserve les trous d'un fichier et ses attributs étendus ;
tout attribut que la destination ne peut conserver est nommé sur la
ligne de message à la fin de l'opération.

Un répertoire copié ou déplacé sur un répertoire de même nom est fusionné
avec lui : son contenu rejoint le répertoire déjà présent, et chacun de ses
fichiers déjà présents fait l'objet de la même question.

L'en-tête de statistiques disque en haut montre le chemin listé, les
octets libres/totaux du volume sous-jacent (quand le service
d'information système peut les rapporter), le nombre d'éléments visibles
et — tant que quelque chose est marqué — le nombre d'entrées marquées. La
bande d'état sous les fenêtres montre la fenêtre active (Tree ou Files),
l'ordre de tri, si les entrées cachées sont affichées, le filtre de nom
de fichier actif, ainsi que le nombre d'entrées marquées et leur total
d'octets. Un fichier dont le format de stockage ne conserve pas de date
de modification affiche `-` dans la colonne de date.

Quand le panneau de fichiers omet des entrées cachées, la session note
aussi l'omission sur le flux d'information standard (fd 3) — un
enregistrement consultatif par changement, capturable avec
`fstree 3>info.jsonl` ; la session elle-même n'en est pas affectée.

Les visionneuses : `Enter` sur un fichier ordinaire l'ouvre en lecture
seule dans une visionneuse plein écran. Un exécutable reconnu — une image
`rxe`, un ELF 64 bits, un module wasm ou un manifeste signé autonome —
s'ouvre dans la **visionneuse de désassemblage** ; un fichier dont les
premiers octets sont sans NUL et en UTF-8 valide s'ouvre dans la
**visionneuse de texte** ; tout le reste s'ouvre dans le
**vidage hexadécimal**. `o` passe outre ce choix. `x` bascule la
visionneuse ouverte vers le vidage hexadécimal au même endroit, `t` vers
la visionneuse de texte (calée au début de la ligne qui contient la
position affichée) et `d` vers la visionneuse de désassemblage (un
fichier qu'aucun format de conteneur ne revendique demande un jeu
d'instructions et se décode comme un fragment brut depuis la position
courante) ; `q` ou `Esc` revient aux panneaux.

Les visionneuses parcourent le fichier par fenêtres bornées — le fichier
n'est jamais tenu entier en mémoire, si bien qu'un fichier de toute
taille (bien au-delà de 4 Gio) se parcourt correctement — et partagent
les touches :

- `Up`/`Down` ou `k`/`j` — une ligne ; `PageUp`/`PageDown`, `b`/`Space`
  — une page ; `Home`/`End` — le début / la dernière page.
- `g` — aller à une position : un numéro de ligne compté à partir de 1
  dans la visionneuse de texte, un décalage en octets (décimal ou
  hexadécimal préfixé par `0x`) dans le vidage hexadécimal, une adresse
  dans la visionneuse de désassemblage.
- `/` — chercher vers l'avant depuis la position courante ; `n` répète
  la dernière recherche au-delà de sa correspondance précédente. Le
  balayage s'exécute en arrière-plan par étapes bornées — la session
  reste réactive sur un fichier énorme, la ligne d'état indique
  `searching…`, et `Esc` l'arrête sur place.
- `?` — cette aide ; toute touche la ferme.

La **visionneuse de texte** décode l'UTF-8 avec un remplacement visible
pour les octets invalides, développe les tabulations et montre tout autre
octet de contrôle sous la forme d'un `·` visible — le contenu des
fichiers n'est jamais envoyé au terminal sous forme de séquences
d'échappement brutes. Les longues lignes sont repliées par défaut ; `w`
active ou désactive le repli (la fin est alors coupée au bord droit).
`/` cherche un texte littéral, sans tenir compte de la casse, par-delà
les limites de lecture. La ligne d'état indique la position sous forme
de numéro de ligne ; après un saut depuis le vidage hexadécimal, le
numéro de ligne est inconnu (on a fait l'économie du comptage) jusqu'à
ce qu'un saut `g` le réancre.

Le **vidage hexadécimal** montre la disposition classique — colonne des
décalages, seize octets en paires hexadécimales, et une colonne ASCII où
les octets non imprimables apparaissent en `.`. La colonne des décalages
est aussi large que le fichier l'exige (au moins huit chiffres
hexadécimaux). `/` cherche un texte littéral (sans tenir compte de la
casse) ou, écrit `0x` suivi de paires d'octets hexadécimaux
(`0xdeadbeef`), une séquence d'octets exacte.

La **visionneuse de désassemblage** s'ouvre sur une page de synthèse :
le format du conteneur, son jeu d'instructions et son point d'entrée,
ses régions (adresse, étendue dans le fichier, taille en mémoire,
permissions) et son nombre de symboles ; un manifeste signé autonome
liste à la place sa version d'ABI et les capacités qu'il demande. Une
image `rxe` porte son manifeste à côté d'elle, jamais intégré, et la
synthèse le dit. `Up`/`Down` parcourent les lignes de régions et `Enter`
ouvre le désassemblage de la région de code sélectionnée (une région de
données s'affiche en hexadécimal sur ses octets de fichier). Un
conteneur qui ne nomme aucun jeu d'instructions — une image `rxe`
s'exécute sur toute machine qui la charge — le demande une fois :
`x` x86-64, `a` aarch64, `r` riscv64, `w` wasm.

Le panneau de code montre une instruction par ligne — adresse, octets
d'encodage, mnémonique, opérandes — avec des lignes d'étiquette
`<symbol>:` et des cibles de branchement symbolisées (`<main+0x8>`) là
où le conteneur nomme des symboles. Les instructions sont décodées écran
par écran, jamais tout le binaire d'avance. `g` saute à une adresse, `/`
cherche dans le texte des instructions, `End` parcourt jusqu'à la fin
de la région en arrière-plan (`Esc` arrête le parcours), et `I` redécode
avec un autre jeu d'instructions. `Esc` revient à la page de synthèse.

Chaque décodage de conteneur et d'instructions s'exécute dans un
processus auxiliaire verrouillé (le bac à sable des analyseurs), jamais
dans le gestionnaire de fichiers lui-même : un exécutable malveillant
peut faire planter l'auxiliaire, pas la session. Un fichier que le
décodeur refuse — mal formé, ou trop grand pour être confié au bac à
sable — retombe sur le vidage hexadécimal avec un avis d'une ligne.

Une lecture que le noyau refuse en cours d'affichage ferme la
visionneuse et affiche l'erreur sur la ligne de message — un contenu
périmé n'est jamais présenté comme actuel.

## OPTIONS

- `directory` — le répertoire de départ de la session ; la valeur par
  défaut est la vue racine `/`.
- `-h`, `-?` — afficher la forme courte de ce document et quitter.

## EXIT STATUS

- `0` — la session s'est terminée par le `q` de l'utilisateur.
- `1` — le répertoire de départ n'a pas pu être listé, ou le chemin du
  terminal a échoué. Une ligne de commande non comprise sort aussi avec
  `1`.

## SEE ALSO

ls, cp, mv, rm, mkdir, chmod, du, df, find
