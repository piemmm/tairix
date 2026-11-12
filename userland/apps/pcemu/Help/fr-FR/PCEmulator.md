## NAME

PCEmulator — émulateur de machines IBM PC et de la classe XT

## SYNOPSIS

`PCEmulator`

## DESCRIPTION

Fait tourner des PC de l'époque du 8088 au 80186 dans des fenêtres de bureau :
de chaque machine, le processeur, la carte d'affichage, les disques, le son et
les ports, à la vitesse qu'avait la machine.

La fenêtre de bibliothèque montre chaque machine sous forme de vignette de son
dernier écran. Nouveau crée une machine d'après l'un de quatre modèles — l'IBM
PC, l'IBM PC XT, un XT turbo et une station de travail 80186 — sous le nom
demandé, avec, sur un modèle doté d'un contrôleur de disque dur, un disque dur
de 30 Mo qui démarre TAIRiX DOS. Démarrer, ou un double-clic, démarre la
machine choisie dans une fenêtre à elle, ou fait passer sa fenêtre au premier
plan ; plusieurs machines tournent à la fois. Réglages change le matériel
d'une machine tant qu'elle est éteinte, Dupliquer copie une machine avec ses
disques, et Supprimer en supprime une avec ses disques une fois la
suppression confirmée. Au premier lancement, PCEmulator crée un XT turbo. Ce
qu'une machine écrit sur ses disques, et l'heure à laquelle son horloge est
réglée, sont conservés d'une exécution à l'autre.

Le XT turbo et la station de travail 80186 ont un coprocesseur numérique 8087,
et Réglages en installe un sur n'importe quelle machine ou le retire. Un
programme qui attend le coprocesseur sans s'être d'abord assuré qu'il y en a
un s'arrête sur une machine qui n'en a pas, comme sur le vrai matériel.

L'image est celle du moniteur, quatre unités de large pour trois de haut,
chaque point de la carte d'affichage gardant ses bords nets quelle que soit la
taille de la fenêtre. Fermer la fenêtre d'une machine la met en veille :
redémarrée, elle reprend là où elle s'était arrêtée, ses disquettes sorties de
leurs lecteurs et son horloge avancée du temps de son absence. Le matériel
d'une machine en veille se change une fois qu'elle a été démarrée puis
éteinte.

Au pied de la fenêtre d'une machine, chacun de ses lecteurs a un voyant qui
s'allume pendant que le lecteur travaille. Appuyer sur un lecteur de
disquettes y propose Insérer une disquette, qui choisit une image de
disquette à mettre dans le lecteur, et Éjecter. Une image de disquette
déposée sur la fenêtre va dans le lecteur sur lequel elle est déposée, ou
sinon dans le premier, et ne peut pas y être modifiée.

Au bout de cette barre, le menu de la machine la met en pause, la
réinitialise, lui envoie Ctrl+Alt+Suppr et règle la vitesse de son processeur
pour la session. Affichage active et désactive les lignes de balayage, la
lueur, la rémanence du phosphore, le bruit et l'ondulation de l'image, et la
machine garde ce qui est choisi. Prendre une capture enregistre l'écran de la
machine comme image PNG là où l'indique le panneau Enregistrer. Plein écran
remplit l'écran avec la fenêtre et laisse la barre en place, où le même menu
rend l'écran. Éteindre éteint la machine et ne garde que son dernier écran.

Copier le texte de l'écran place dans le presse-papiers le texte que montre
l'écran de la machine, tant qu'il montre du texte. Coller tape le texte du
presse-papiers sur le clavier de la machine, chaque caractère une fois que le
programme a lu le précédent. Ce que la machine ne peut pas taper tel quel — les
visages et les flèches du PC, ou une lettre qu'elle ne connaît pas — est laissé
de côté, et la fenêtre le dit. Arrêter de coller, ou une touche pressée dans la
fenêtre de la machine, arrête la frappe.

Partager un dossier prête à la machine un de vos dossiers, choisi dans le
sélecteur, comme un lecteur à part entière : la première lettre que ses
disques laissent libre, une fois TAIRiX DOS démarré. Un nom que DOS ne peut
pas contenir est raccourci, comme Windows raccourcit un nom long, et un
dossier partagé en lecture seule, ou un fichier que vous ne pouvez pas
modifier, ne peut pas être modifié depuis la machine. Chaque dossier partagé a
une place sur la barre qui nomme son lecteur ; appuyer dessus propose Arrêter
le partage. Une machine partage jusqu'à quatre dossiers, et plus aucun une
fois mise en veille ou éteinte.

Débogueur, dans le menu de la machine, ouvre une fenêtre sur la machine : ses
registres, son 8087, son code et sa mémoire tels qu'ils étaient à son arrêt,
et ses points d'arrêt. Interrompre arrête la machine là où elle en est ;
Continuer, Pas à pas détaillé, Pas à pas principal et Pas à pas sortant la
font repartir, comme F5, F11, F10 et Maj+F11, F5 arrêtant aussi une machine
qui tourne. Appuyer à côté d'une ligne de code y place un point d'arrêt ou
l'enlève, comme F9 le fait pour la ligne courante, et Ajouter un point d'arrêt
arrête plutôt la machine quand elle écrit ou lit la mémoire, lit ou écrit un
port, ou entre dans une interruption. Le temps de la machine reste figé tant
qu'elle est arrêtée, et fermer le débogueur la laisse repartir.

Tant que la fenêtre d'une machine détient le clavier, chaque touche va à la
machine comme une touche de son propre clavier, Ctrl, Alt et les touches de
verrouillage comprises ; le bureau garde Ctrl+Alt+Suppr, ses raccourcis de
capture et la touche Méta. Un programme qui utilise la souris par le pilote de
souris du DOS suit le pointeur du bureau partout où il se trouve sur l'image.
Un programme qui lit la souris lui-même la prend quand on clique sur l'image,
et la rend quand on appuie brièvement sur la touche Méta.

Fermer la fenêtre de bibliothèque laisse tourner les machines en marche, et
Bibliothèque de machines, dans le menu de la barre d'icônes, la rouvre. Chaque
machine tourne dans un processus de travail à part qui ne détient aucune
autorité : ses logiciels et ses disques ne sont jamais considérés comme
fiables.

## OPTIONS

`-h`, `-?`, `--help`
: Écrire cette aide sur la sortie standard et quitter.

## EXIT STATUS

Zéro une fois sa dernière fenêtre fermée. Non nul lorsque le canal de fenêtre, la
boîte aux lettres d'événements ou la session de bureau a été refusée ; la
raison est énoncée sur la sortie d'erreur standard.
