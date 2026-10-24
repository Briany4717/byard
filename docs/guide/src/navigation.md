# Navigation

```byld
View Main() {
    var path = "/"

    NavStack(path: path) #[grow: 1, transition: slide, swipe_back: true] {
        route "/" {
            Column #[bg: 0x14141C, p: 24, gap: 12, grow: 1] {
                Text("Library") #[color: 0xE6E1E5, size: 22]
                Button("Piranesi") => path = "/book/42"
                Button("Gormenghast") => path = "/book/7"
            }
        }
        route "/book/:id" {|params|
            Column #[bg: 0x1A1620, p: 24, gap: 12, grow: 1] {
                Text("Book #{params.id}") #[color: 0xE6E1E5, size: 22]
                Button("Back") => back(path)
            }
        }
    }
}
```

Navigation is a `var`. There is no navigation object, no route handle and no
controller to hold. The `var` holds a path, like `"/book/42"`. A `NavStack` shows the
screen that matches it. Setting the `var` is the push. Going back is `back(path)`.

## NavStack

`NavStack(path: somevar)` shows one screen at a time from a list of *routes* and keeps
a history of the paths it has been given. Each `route "pattern" { … }` is a screen.

A pattern is a path. A segment starting with `:` matches anything and is handed to the
screen by name: the `{|params| … }` after the brace binds all the matches as a
record, so `/book/:id` gives `params.id`. Parameters are always `Str`.

```byld,fragment
var path = "/"
NavStack(path: path) {
    route "/" { Text("home") }
    route "/user/:name/posts/:post" {|params|
        Text("{params.name}, post {params.post}")
    }
    route "*" {
        Text("No route matched {route.path}")
    }
}
```

`route "*"` is a catch-all: it takes whatever the others did not. Inside a screen,
`route.path` is the concrete path it was made for. A malformed pattern is an error
at check time, and so is an element placed in a `NavStack` that is not a `route`.

Each concrete path is its own screen. Pushing `/book/2` while `/book/1` is open puts a
second one on top of the first. The first is not unmounted. It is kept underneath
with its state, so when you go back, its scroll position, its text fields and its
counters are exactly as you left them.

### Moving

| Write | Does |
|-------|------|
| `path = "/book/2"` | Pushes that screen |
| `navigate(path, "/book/2")` | The same, as a call |
| `back(path)` | Pops one screen. At the root it does nothing |
| `replace(path, "/saved")` | Swaps in a screen without stacking, so one Back skips the one it replaced |
| `path = "/"` | Pops all the way to the path, dropping what it skipped |

`replace` is for a screen you should not come back to, such as an edit form after it
saves. The first argument of `back`, `replace` and `navigate` is the `var` that
drives the stack.

### Transitions and gestures

`transition` says how one screen swaps for the next.

| Value | Looks like |
|-------|------------|
| `slide` | A push: the new screen slides in from the right while the old one drifts left |
| `slide_up` | A sheet rising over a screen that stays put |
| `fade` | A cross-fade |
| `none` | An instant swap |

`swipe_back: true` lets the user drag from the left edge to pop. The screen follows
the finger, completes if it passes halfway, and springs back if not. What is under
the finger is the real previous screen, already there, not a picture of it.
`max_depth` limits how deep a stack may grow (10 by default, `0` for no limit), which
catches a screen that keeps pushing itself.

`route_change => …` runs once a navigation settles.

## Tabs with NavHost

A `NavHost` holds several screens and shows one, chosen by a `var` that names it. Its
cases are `tab "name"` blocks:

```byld
View Main() {
    var tab = "home"
    var homePath = "/"
    var likes = 0

    let tabButton = style {
        radius: 20, p: (10, 20), bg: 0x1D1B24, color: 0x938F99
        on selected { bg: 0x381E72, color: 0xD0BCFF }
    }

    Column #[bg: 0x0F0E13, grow: 1] {
        NavHost(active: tab) #[grow: 1, transition: fade] {
            tab "home" {
                NavStack(path: homePath) #[grow: 1, transition: slide, swipe_back: true] {
                    route "/" {
                        Column #[p: 24, gap: 12, grow: 1] {
                            Text("Home") #[color: 0xE6E1E5, size: 22]
                            Button("♥ {likes}") => likes++
                            Button("Details") => homePath = "/details"
                        }
                    }
                    route "/details" {
                        Column #[p: 24, gap: 12, grow: 1] {
                            Text("Details") #[color: 0xE6E1E5, size: 22]
                            Button("Back") => back(homePath)
                        }
                    }
                }
            }
            tab "settings" {
                Column #[p: 24, grow: 1] {
                    Text("Settings") #[color: 0xE6E1E5, size: 22]
                }
            }
        }

        Row #[bg: 0x1D1B24, p: 10, gap: 10, justify: center] {
            Button("Home") #[..tabButton, selected: tab == "home"] => tab = "home"
            Button("Settings") #[..tabButton, selected: tab == "settings"] => tab = "settings"
        }
    }
}
```

The tab bar is ordinary buttons writing an ordinary `var`, and `selected:` makes the
active one style itself with `on selected`. The `NavHost` fades between tabs by
default, because a tab switch has no direction. Each tab keeps its state when you
leave and return: the likes counter above survives a trip to Settings, and a tab
with a stack inside keeps its history.

Stacks and hosts nest freely. Give every stack its own `var`, and each keeps its own
history, the shape of an app with a tab bar over a stack per tab.

## Deep links

`deep_link: true` on a `NavStack` lets it accept URLs from the operating system, the
way a link opens a mobile app on a particular screen. The URL's path goes straight
through that stack's routes:

```sh
byard dev --deep-link byard://book/42
```

opens the app already on `/book/42`. A deep link reaches every *mounted* stack with
`deep_link: true`, so the stack that should take one belongs in the tab the app
starts on.

## Back and the keyboard

`back` is an action like any other, so you can put it wherever you like: a button, a
`key_down` handler, a link in a header. Byard has no system back button, because the
hosts that exist are desktop ones. When there is one, it will call the same action.

## What does not cost anything

A navigation that is not moving costs no frames: when nothing is animating the app
parks, and a transition costs frames only while it runs. Screens you are not looking
at are kept, not redrawn.
