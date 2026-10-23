## NAME

settings — configurer le bureau et cette machine

## SYNOPSIS

`settings`

## DESCRIPTION

Ouvre une fenêtre de bureau qui répertorie chaque catégorie de réglages de ce
système : ce qu'est la machine, l'apparence du bureau, l'écran, le réseau, les
périphériques de commande, les comptes qui l'utilisent et les volumes qu'elle
contient. Choisir une catégorie dans la barre latérale affiche son panneau. Une
catégorie qui contient plusieurs panneaux ouvre et ferme plutôt leur liste sur
place, et autant de ces listes que voulu peuvent rester ouvertes à la fois.

Settings ne détient aucune autorité propre. Chaque modification est soit une
demande à la session de bureau, qui possède les réglages de l'utilisateur, soit
une exécution ré-authentifiée de la commande qui écrit déjà ce magasin : rien
ici ne peut élever un privilège.

Une catégorie que ce système ne peut pas servir le dit clairement et nomme ce
qui devrait exister pour qu'elle le puisse. Un contrôle qui ne changerait rien
n'est jamais montré.

Saisissez du texte dans le champ de recherche au-dessus de la barre latérale
pour la filtrer sur les catégories et les réglages qu'un mot atteint. `Tab` et
`Shift+Tab` déplacent le focus entre le champ de recherche, le fil de
navigation, la barre latérale et le panneau ; `Up` et `Down` parcourent la
barre latérale et `Enter` ouvre la ligne. `Right` et `Left` ouvrent et ferment
la liste de la catégorie sous le curseur. Une fenêtre trop étroite abandonne la
barre latérale, et la première miette du fil de navigation liste alors les
catégories.

Il se lance depuis la ligne *Settings…* du menu système du bureau, depuis la
Bibliothèque de programmes, ou par son nom depuis un shell. Il exige une
session graphique en cours : sans elle, le canal de fenêtre est inaccessible et
il signale le refus sur le flux d'erreur standard puis se termine.

Son comporte deux panneaux. *Output & Input* choisit le périphérique par lequel
le son est joué et celui depuis lequel il est enregistré, et règle le volume et
la sourdine de ce périphérique : la sourdine baisse le volume à rien jusqu'à ce
que vous la retiriez, ce qui le remet où il était. Il montre aussi ce qui
enregistre : vos propres enregistrements par leur nom, et combien en font les
autres. Ces réglages changent le périphérique sur-le-champ, et seulement tant
que votre session tient la salle qu'il dessert ; un périphérique de la salle
d'une autre session est affiché mais ne peut pas être modifié ici. Votre bureau
retient ce que vous réglez. *Sound Effects* active ou coupe les sons propres du
bureau, règle leur thème et leur volume et, pour chaque événement — une
notification, une erreur, la cloche du terminal —, indique s'il joue le son de
son thème, un fichier que vous choisissez ou rien. Les sons de démarrage,
d'ouverture et de fermeture de session appartiennent à la machine et sont
fixés par son administrateur.

**Raspberry Pi** n'apparaît que si la machine en est un. On y règle le
ventilateur du processeur, commuté par une ligne du connecteur à 40 broches :
*Off*, *On* en permanence ou *Automatic*, qui met le ventilateur en marche dès
que le processeur atteint la température choisie et ne l'arrête qu'une fois
qu'il s'est refroidi de l'écart choisi, pour qu'il ne bascule pas sans cesse
autour d'un même degré. Vous choisissez la ligne de commande par son numéro GPIO
et sa broche, ainsi que le niveau, haut ou bas, auquel le ventilateur tourne ;
une ligne déjà utilisée par autre chose est nommée plutôt que proposée. Une
ligne du connecteur ne peut pas alimenter un ventilateur à elle seule :
branchez-le par l'intermédiaire d'un transistor. Le panneau indique aussi la
température du processeur et si le ventilateur tourne. Quand *Automatic* ne peut
pas lire la température, le ventilateur tourne jusqu'à ce qu'il le puisse. Comme
les autres réglages de la machine, une modification attend *Apply*, qui demande
un compte autorisé à les modifier.

## EXIT STATUS

Zéro après une fermeture propre ; non nul lorsque le canal de fenêtre ou la
région de trames partagée a été refusé (la raison est indiquée sur le flux
d'erreur standard).
