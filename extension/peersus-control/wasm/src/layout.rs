//! The extension's desktop editor. The host only provides scoped geometry and generic elements.
use crate::{host::Wire, remote::{State, answer}};
use serde::Deserialize;
use serde_json::{Value, json};
const INITIAL_HEIGHT: f64 = 360.;
#[derive(Clone, Deserialize, PartialEq)]
struct Display { x:i32, y:i32, width:u32, height:u32 }
#[derive(Clone, Deserialize, PartialEq)]
struct Item { device:String, name:String, displays:Vec<Display>, at:Option<[i64;2]>, local:bool }
#[derive(Clone)]
struct Rect { device:usize, x:i64,y:i64,w:i64,h:i64 }
impl Rect { fn right(&self)->i64 {self.x+self.w} fn bottom(&self)->i64 {self.y+self.h} }
#[derive(Clone)]
enum Grab {
    Pan {from:[f64;2],was:[f64;2]},
    Device {index:usize,from:[f64;2],was:[i64;2]},
    Menu {index:usize,from:[f64;2],was:Option<[i64;2]>,moved:bool},
}
#[derive(Default)]
pub struct Ui {
    items:Vec<Item>, expected:Value, zoom:Option<(f64,[f64;2])>, grab:Option<Grab>,
    bounds:[f64;4], refreshed:u64, error:Option<String>,
    /// The canvas of each page this is drawn on, by page: the same layout is shown in the settings
    /// and, when asked, as a page of its own, each of its own size.
    pages:std::collections::BTreeMap<String,[f64;4]>,
    /// The page the gesture in progress is on.
    grab_page:Option<String>,
    choosing:bool, menu_page:usize, pointer:Option<[f64;2]>, rendered_at:Option<u64>,
    menu_anchor:Option<[f64;2]>,
}
fn text(s:impl Into<String>)->Value {json!({"k":"text","text":s.into(),"tone":"muted"})}
fn px(n:f64)->Value {json!({"unit":"px","value":n.round().clamp(0.,8192.) as u16})}
fn fill()->Value {json!({"unit":"percent","value":10000})}
fn bx(id:Option<&str>,style:Value,children:Vec<Value>)->Value {json!({"k":"box","id":id,"style":style,"children":children})}
fn button(id:impl Into<String>,label:impl Into<String>)->Value {json!({"k":"button","id":id.into(),"label":label.into()})}
impl Item {
    fn screens(&self,index:usize)->Vec<Rect> {
        let Some(at)=self.at else {return vec![]};
        if self.displays.is_empty() {return vec![Rect {device:index,x:at[0],y:at[1],w:1920,h:1080}]}
        let left=self.displays.iter().map(|d|d.x).min().unwrap_or(0);
        let top=self.displays.iter().map(|d|d.y).min().unwrap_or(0);
        self.displays.iter().map(|d|Rect {device:index,x:at[0]+i64::from(d.x)-i64::from(left),y:at[1]+i64::from(d.y)-i64::from(top),w:i64::from(d.width),h:i64::from(d.height)}).collect()
    }
    fn extent(&self)->[i64;2] {
        if self.displays.is_empty() {return [1920,1080]}
        let left=self.displays.iter().map(|d|i64::from(d.x)).min().unwrap_or(0);
        let top=self.displays.iter().map(|d|i64::from(d.y)).min().unwrap_or(0);
        [self.displays.iter().map(|d|i64::from(d.x)+i64::from(d.width)).max().unwrap_or(0)-left,
            self.displays.iter().map(|d|i64::from(d.y)+i64::from(d.height)).max().unwrap_or(0)-top]
    }
}
impl Ui {
    fn load(&mut self,value:Value)->Result<(), &'static str> {
        let items:Vec<Item>=serde_json::from_value(value["devices"].clone()).map_err(|_|answer::NOT_UNDERSTOOD)?;
        if items.len()>65 || items.iter().any(|i|i.displays.len()>16) || !value["placements"].is_array() {return Err(answer::NOT_UNDERSTOOD)}
        // Every paired device can be placed: one whose screens are not known yet is drawn at a
        // default size ("size not known yet") until it says, never left out of the editor.
        self.items=items;
        self.expected=value["placements"].clone();
        Ok(())
    }
    fn refresh<H:Wire>(&mut self,state:&mut State,host:&mut H)->Result<(), &'static str> {
        self.load(state.call(host,"peersus.displays","inspect",json!({}))?)
    }
    fn save<H:Wire>(&mut self,state:&mut State,host:&mut H)->Result<(), &'static str> {
        let placements:Vec<Value>=self.items.iter().filter(|i|!i.local).filter_map(|i|i.at.map(|at|json!({"device":i.device,"x":at[0],"y":at[1]}))).collect();
        let value=state.call(host,"peersus.displays","arrange",json!({"expected":self.expected,"placements":placements}))
            .map_err(|reason| if reason==answer::CANCELLED {reason}else{"The layout could not be saved. Check this extension's display-arrangement permission and reload before retrying."})?;
        self.load(value)
    }
    fn monitors(&self)->Vec<Rect> { self.items.iter().enumerate().flat_map(|(n,i)|i.screens(n)).collect() }
    fn height(&self)->f64 {self.bounds[3].max(1.)}
    fn view(&self)->(f64,[f64;2]) {
        self.zoom.unwrap_or_else(|| {
            let all=self.monitors(); let w=self.bounds[2].max(1.);
            let l=all.iter().map(|r|r.x).min().unwrap_or(0) as f64;
            let t=all.iter().map(|r|r.y).min().unwrap_or(0) as f64;
            let r=all.iter().map(Rect::right).max().unwrap_or(1920) as f64;
            let b=all.iter().map(Rect::bottom).max().unwrap_or(1080) as f64;
            let sx=(r-l).max(1.);let sy=(b-t).max(1.);
            let scale=(w/(sx*1.3)).min(self.height()/(sy*1.3)).clamp(0.005,2.);
            (scale,[l-(w/scale-sx)/2.,t-(self.height()/scale-sy)/2.])
        })
    }
    fn verdict(&self)->(bool,String) {
        let all=self.monitors();
        // Only devices may not overlap each other: a device's own screens may (mirrored outputs share one place).
        for (n,a) in all.iter().enumerate() {for b in all[n+1..].iter().filter(|b|b.device!=a.device) {
            if a.right().min(b.right())-a.x.max(b.x)>2 && a.bottom().min(b.bottom())-a.y.max(b.y)>2 {
                return (false,format!("{} and {} overlap: move one of them.",self.items[a.device].name,self.items[b.device].name))
            }
        }}
        let local=self.items.iter().position(|i|i.local);
        for (index,item) in self.items.iter().enumerate().filter(|(_,i)|!i.local && i.at.is_some()) {
            let touches=all.iter().filter(|r|r.device==index).any(|a|all.iter().filter(|r|Some(r.device)==local).any(|b| {
                ((a.right().abs_diff(b.x)<=2 || b.right().abs_diff(a.x)<=2) && a.bottom().min(b.bottom())>a.y.max(b.y)) ||
                ((a.bottom().abs_diff(b.y)<=2 || b.bottom().abs_diff(a.y)<=2) && a.right().min(b.right())>a.x.max(b.x))
            }));
            if !touches {return (true,format!("{} does not touch this computer, so the pointer never reaches it.",item.name))}
        }
        (true,"Every device touches this one: the pointer crosses through the shared edge.".into())
    }
    fn snap(&self,index:usize,wanted:[i64;2],scale:f64)->[i64;2] {
        let size=self.items[index].extent();let threshold=(16./scale) as u64;
        let mut best:[Option<(u64,i64)>;2]=[None,None];
        for other in self.monitors().iter().filter(|r|r.device!=index) {
            for (axis,candidates) in [(0,[other.right(),other.x-size[0],other.x,other.right()-size[0]]),(1,[other.bottom(),other.y-size[1],other.y,other.bottom()-size[1]])] {
                for candidate in candidates {let distance=candidate.abs_diff(wanted[axis]);if distance<=threshold && best[axis].is_none_or(|(d,_)|distance<d) {best[axis]=Some((distance,candidate));}}
            }
        }
        [best[0].map_or(wanted[0],|(_,n)|n),best[1].map_or(wanted[1],|(_,n)|n)]
    }
    fn menu_height(&self)->f64 {
        let count=self.items.iter().filter(|item|!item.local).count();
        let page=self.menu_page.min(count.saturating_sub(1)/4);
        if self.choosing {52.+count.saturating_sub(page*4).min(4) as f64*48.+if count>4 {44.}else{0.}}else{44.}
    }
    fn menu_position(&self, height:f64)->[f64;2] {
        if self.choosing && let Some(at)=self.menu_anchor {return at}
        let width=self.bounds[2].max(1.);let menu_width=220f64.min((width-16.).max(1.));
        let (scale,origin)=self.view();
        let overlap=|at:[f64;2]| {
            let mut area=0.;
            for item in &self.items {
                let Some(point)=item.at else {continue};
                let size=item.extent();
                let (x,y)=((point[0] as f64-origin[0])*scale,(point[1] as f64-origin[1])*scale);
                area+=((at[0]+menu_width).min(x+size[0] as f64*scale)-at[0].max(x)).max(0.)
                    *((at[1]+height).min(y+size[1] as f64*scale)-at[1].max(y)).max(0.);
            }
            if self.pointer.is_some_and(|p|p[0]>=at[0]-24. && p[0]<=at[0]+menu_width+24. && p[1]>=at[1]-24. && p[1]<=at[1]+height+24.) {area+=menu_width*height;}
            area
        };
        let bottom=(self.height()-height-8.).max(8.);
        let candidates=[[8.,8.],[(width-menu_width-8.).max(8.),8.],[8.,bottom],[(width-menu_width-8.).max(8.),bottom]];
        candidates.into_iter().min_by(|a,b|overlap(*a).total_cmp(&overlap(*b))).unwrap()
    }
    pub fn event<H:Wire>(&mut self,state:&mut State,host:&mut H,event:&Value,page:Option<&str>)->Result<Value,&'static str> {
        // One layout is shown on more than one page. A gesture is on one of them: while it lasts, the
        // other page's ticks and measurements must not swap the canvas under it.
        if self.grab.is_some() && page.is_some() && self.grab_page.as_deref()!=page {return Ok(Value::Null)}
        if let Some(bounds)=page.and_then(|page|self.pages.get(page)) {self.bounds=*bounds;}
        let id=event["id"].as_str().unwrap_or("");
        let (scale,origin)=self.view();
        match event["t"].as_str() {
            Some("open")=> {if self.bounds[2]<=0. {self.bounds[2]=600.;}if self.bounds[3]<=0. {self.bounds[3]=INITIAL_HEIGHT;}self.refresh(state,host)?;},
            Some("tick")=> {let at=event["at"].as_u64().unwrap_or(0);if self.grab.is_some() || at.saturating_sub(self.refreshed)<2000 {return Ok(Value::Null)}self.refreshed=at;let before=self.items.clone();self.refresh(state,host)?;if before==self.items {return Ok(Value::Null)}},
            Some("size") if id=="canvas"=> {
                let next=[event["x"].as_f64().unwrap_or(0.)/1000.,event["y"].as_f64().unwrap_or(0.)/1000.,
                    event["width"].as_f64().unwrap_or(600000.)/1000.,event["height"].as_f64().unwrap_or(INITIAL_HEIGHT*1000.)/1000.];
                let resized=(next[2]-self.bounds[2]).abs()>=0.5 || (next[3]-self.bounds[3]).abs()>=0.5;
                self.bounds=next;
                if let Some(page)=page {self.pages.insert(page.to_owned(),next);}
                // Where the canvas sits only matters for the wheel; a new page answered for every move
                // or scroll of it would be measured and answered again, a redraw on every frame.
                if !resized {return Ok(Value::Null)}
            },
            Some("wheel") if id=="canvas"=> {
                let dy=event["dy"].as_i64().unwrap_or(0);if dy==0 {return Ok(Value::Null)}
                let next=(scale*if dy>0 {1.15}else{1./1.15}).clamp(0.005,2.);
                let x=event["x"].as_f64().unwrap_or(0.)/1000.-self.bounds[0];let y=event["y"].as_f64().unwrap_or(0.)/1000.-self.bounds[1];
                self.zoom=Some((next,[origin[0]+x/scale-x/next,origin[1]+y/scale-y/next]));
            },
            Some("press")=>match id {
                "fit"=>self.zoom=None,"minus"=>self.zoom=Some(((scale/1.25).clamp(0.005,2.),origin)),"plus"=>self.zoom=Some(((scale*1.25).clamp(0.005,2.),origin)),
                "devices"=> {
                    let at=self.menu_position(self.menu_height());
                    self.choosing = !self.choosing;
                    self.menu_anchor=if self.choosing {Some([at[0],at[1].min((self.height()-self.menu_height()-8.).max(8.))])}else{None};
                },
                "devices-next"=>self.menu_page+=1,
                "devices-previous"=>self.menu_page=self.menu_page.saturating_sub(1),
                _=>if let Some(index)=id.strip_prefix("remove-").and_then(|n|n.parse::<usize>().ok()).filter(|n|*n<self.items.len() && !self.items[*n].local && self.items[*n].at.is_some()) {
                    let old=self.items[index].at;
                    self.error=None;
                    self.items[index].at=None;
                    if let Err(reason)=self.save(state,host) {self.items[index].at=old;self.error=Some(reason.into());}
                }else{return Ok(Value::Null)},
            },
            Some("pointer") if id=="device-menu"=>return Ok(Value::Null),
            Some("pointer")=> {
                let menu_at=if event["phase"]=="down" {self.menu_position(self.menu_height())}else{[0.,0.]};
                let point=[event["x"].as_f64().unwrap_or(0.)/1000.,event["y"].as_f64().unwrap_or(0.)/1000.];
                self.pointer=Some([point[0]-self.bounds[0],point[1]-self.bounds[1]]);
                match event["phase"].as_str() {
                    Some("down")=> {
                        self.error=None;self.zoom=Some((scale,origin));
                        self.grab=if let Some(index)=id.strip_prefix("menu-device-").and_then(|n|n.parse::<usize>().ok()).filter(|n|*n<self.items.len() && !self.items[*n].local) {
                            self.menu_anchor=Some(menu_at);
                            Some(Grab::Menu{index,from:point,was:self.items[index].at,moved:false})
                        }else if let Some(index)=id.strip_prefix("desktop-").and_then(|n|n.parse::<usize>().ok()).filter(|n|*n<self.items.len() && !self.items[*n].local) {
                            self.items[index].at.map(|was|Grab::Device{index,from:point,was})
                        }else if id=="canvas" {Some(Grab::Pan{from:point,was:origin})}else{None};
                        self.grab_page=self.grab.as_ref().and(page.map(str::to_owned));
                        return Ok(Value::Null)
                    },
                    Some("move")=> {
                        match self.grab.clone() {
                            Some(Grab::Pan{from,was})=> {
                                let next=[was[0]-(point[0]-from[0])/scale,was[1]-(point[1]-from[1])/scale];
                                if origin==next {return Ok(Value::Null)}
                                self.zoom=Some((scale,next));
                            },
                            Some(Grab::Device{index,from,was})=> {
                                let wanted=[was[0]+((point[0]-from[0])/scale) as i64,was[1]+((point[1]-from[1])/scale) as i64];
                                let next=Some(self.snap(index,wanted,scale));
                                if self.items[index].at==next {return Ok(Value::Null)}
                                self.items[index].at=next;
                            },
                            Some(Grab::Menu{index,from,was,moved})=> {
                                let moved=moved || (point[0]-from[0]).hypot(point[1]-from[1])>=6.;
                                if !moved {return Ok(Value::Null)}
                                let size=self.items[index].extent();
                                let wanted=[(origin[0]+(point[0]-self.bounds[0])/scale-size[0] as f64/2.).round() as i64,
                                    (origin[1]+(point[1]-self.bounds[1])/scale-size[1] as f64/2.).round() as i64];
                                self.items[index].at=Some(self.snap(index,wanted,scale));
                                self.grab=Some(Grab::Menu{index,from,was,moved});
                            },
                            None=>return Ok(Value::Null),
                        }
                        // Track every sample, but serialize at most one visual frame per 16ms.
                        // Up/cancel always renders the final state, even inside this interval.
                        if let Some(at)=event["at"].as_u64() {
                            if self.rendered_at.is_some_and(|last|at.saturating_sub(last)<16) {return Ok(Value::Null)}
                            self.rendered_at=Some(at);
                        }
                    },
                    Some("up"|"cancel")=> {
                        self.grab_page=None;
                        match self.grab.take() {
                            Some(Grab::Device{index,was,..})=> {
                                if event["phase"]=="cancel" || !self.verdict().0 {self.items[index].at=Some(was)}
                                else if self.items[index].at!=Some(was) && let Err(reason)=self.save(state,host) {self.items[index].at=Some(was);self.error=Some(reason.into());}
                            },
                            Some(Grab::Menu{index,was,moved,..})=> {
                                let inside=point[0]>=self.bounds[0] && point[0]<=self.bounds[0]+self.bounds[2]
                                    && point[1]>=self.bounds[1] && point[1]<=self.bounds[1]+self.height();
                                if event["phase"]=="cancel" || (moved && !inside) {
                                    self.items[index].at=was;
                                }else {
                                    if !moved && was.is_none() {
                                        self.items[index].at=Some([self.monitors().iter().map(Rect::right).max().unwrap_or(0),0]);
                                    }
                                    if !self.verdict().0 {self.items[index].at=was}
                                    else if self.items[index].at!=was && let Err(reason)=self.save(state,host) {self.items[index].at=was;self.error=Some(reason.into());}
                                }
                                self.choosing=false;
                                self.menu_anchor=None;
                            },
                            _=>{},
                        }
                    },
                    _=>return Err(answer::NOT_UNDERSTOOD),
                }
            },
            _=>return Err(answer::NOT_UNDERSTOOD),
        }
        Ok(self.tree())
    }
    pub fn tree(&self)->Value {
        let (scale,origin)=self.view();let width=self.bounds[2].max(1.);
        let step=[100.,250.,500.,1000.,2000.,5000.,10000.,20000.,50000.,100000.].into_iter().find(|n|n*scale>=64.).unwrap_or(100000.);
        let mut canvas=Vec::new();
        for (axis, offset) in origin.iter().enumerate() {
            let limit=if axis==0 {width}else{self.height()};let first=(*offset/step).floor();
            for n in 0..=((limit/scale/step).ceil() as usize+1).min(128) {
                let plane=(first+n as f64)*step;let pos=(plane-*offset)*scale;if pos<0. || pos>limit {continue}
                let at=if axis==0 {[(pos*1000.) as i32,0]}else{[0,(pos*1000.) as i32]};
                canvas.push(bx(None,json!({"at":at,"width":if axis==0 {px(1.)}else{fill()},"height":if axis==0 {px(self.height())}else{px(1.)},"background":if plane==0. {"muted"}else{"border"}}),vec![]));
                if pos>36. {let label_at=if axis==0 {[(pos*1000.) as i32+4000,4000]}else{[4000,(pos*1000.) as i32+2000]};canvas.push(bx(None,json!({"at":label_at}),vec![text(format!("{plane:.0}"))]));}
            }
        }
        let (savable,verdict)=self.verdict();
        for (index,item) in self.items.iter().enumerate() {
            let Some(at)=item.at else{continue};let size=item.extent();let x=(at[0] as f64-origin[0])*scale;let y=(at[1] as f64-origin[1])*scale;
            let w=(size[0] as f64*scale).max(24.);let h=(size[1] as f64*scale).max(24.);
            if x+w<0. || y+h<0. || x>width || y>self.height() {continue}
            // Send only the visible rectangle: zooming into a large monitor must not exceed the
            // generic element coordinate/size bounds or invalidate the rest of the page.
            let (left,top,right,bottom)=(x.max(0.),y.max(0.),(x+w).min(width),(y+h).min(self.height()));
            let (x,y,w,h)=(left,top,(right-left).max(1.),(bottom-top).max(1.));
            let id=format!("desktop-{index}");
            // Only what fits: a small box shows the name, a larger one the picture, the largest the place and size.
            let mut inside=Vec::new();
            if h>=72. && w>=56. {inside.push(json!({"k":"avatar","device":item.device,"name":item.name,"size":if h>=96. && w>=96. {32}else{22}}));}
            inside.push(json!({"k":"text","text":item.name}));
            if item.local && h>=56. {inside.push(text(format!("{}, {}",at[0],at[1])));}
            // A coloured outline (the outer box) around a dark face: the state shows without hiding the picture.
            let edge=if !savable {"danger"}else if item.local {"accent"}else{"success"};
            let face=bx(None,json!({"width":fill(),"height":fill(),"radius":[5,5,5,5],"background":"surface","center":true,"gap":4,"padding":2,"clip":true}),inside);
            canvas.push(bx((!item.local).then_some(id.as_str()),json!({"at":[(x*1000.) as i32,(y*1000.) as i32],"width":px(w),"height":px(h),"radius":[6,6,6,6],"background":edge,"padding":2,"clip":true}),vec![face]));
        }
        let peers:Vec<_>=self.items.iter().enumerate().filter(|(_,item)|!item.local).collect();
        let page=self.menu_page.min(peers.len().saturating_sub(1)/4);
        let shown=peers.iter().skip(page*4).take(4).collect::<Vec<_>>();
        let height=self.menu_height();
        let at=self.menu_position(height);
        let mut menu=vec![button("devices",if self.choosing {"Devices ▴"}else{"Devices ▾"})];
        if self.choosing {
            for (n,item) in shown {
                let mut row=vec![json!({"k":"avatar","device":item.device,"name":item.name,"size":28}),
                    json!({"k":"text","text":item.name})];
                if item.at.is_some() {row.push(button(format!("remove-{n}"),"×"));}
                menu.push(bx(Some(&format!("menu-device-{n}")),json!({"horizontal":true,"gap":8,"height":px(40.),"width":fill()}),row));
            }
            if peers.is_empty() {menu.push(text("No paired devices."));}
            if peers.len()>4 {
                let mut navigation=Vec::new();
                if page>0 {navigation.push(button("devices-previous","Previous"));}
                if (page+1)*4<peers.len() {navigation.push(button("devices-next","Next"));}
                menu.push(bx(None,json!({"horizontal":true,"gap":8}),navigation));
            }
        }
        canvas.push(bx(Some("device-menu"),json!({"at":[(at[0]*1000.) as i32,(at[1]*1000.) as i32],"width":px(220f64.min((width-16.).max(1.))),"padding":8,"gap":8,"radius":[10,10,10,10],"background":"raised","border":true}),menu));
        let mut plane=bx(Some("canvas"),json!({"width":fill(),"height":{"unit":"flex","value":1},"clip":true,"radius":[12,12,12,12],"border":true,"background":"surface"}),canvas);
        plane["observe"]=json!(["size","wheel"]);
        let children=vec![bx(None,json!({"width":fill(),"horizontal":true,"gap":8}),vec![text("Layout"),button("minus","−"),button("plus","+"),button("fit","Zoom to fit")]),
            text("Arrange devices around this computer. Click a device in Devices to place it, or drag it from the menu to the canvas. Drag placed devices to move them; drag the background to pan and use the wheel to zoom."),plane,text(self.error.as_ref().unwrap_or(&verdict))];
        json!({"k":"interval","id":"layout-clock","milliseconds":1000,"child":bx(None,json!({"width":fill(),"height":fill(),"padding":12,"gap":8}),children)})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::scripted::Script;

    fn scene() -> Value {
        json!({"devices":[
            {"device":"this-device","name":"Local","displays":[{"x":-100,"y":-50,"width":100,"height":100}],"at":[0,0],"local":true},
            {"device":"scoped","name":"Remote","displays":[{"x":0,"y":0,"width":100,"height":100}],"at":[100,0],"local":false}
        ],"placements":[{"device":"scoped","x":100,"y":0}]})
    }
    fn ready() -> Ui {
        let mut ui=Ui::default();
        ui.load(scene()).unwrap();
        ui.bounds=[10.,20.,600.,INITIAL_HEIGHT];
        ui.zoom=Some((1.,[0.,0.]));
        ui
    }
    fn pointer(ui:&mut Ui,state:&mut State,host:&mut Script,id:&str,phase:&str,x:i64,y:i64)->Value {
        ui.event(state,host,&json!({"t":"pointer","id":id,"phase":phase,"x":x,"y":y}),None).unwrap()
    }

    #[test]
    fn height_only_resize_refits_the_plane_and_canvas_uses_the_available_area() {
        let mut ui=ready();let mut state=State::default();let mut host=Script::default();
        ui.zoom=None;ui.bounds[2]=2000.;ui.items[0].displays[0].height=1000;
        let before=ui.view().0;
        let tree=ui.event(&mut state,&mut host,&json!({"t":"size","id":"canvas","x":10000,"y":20000,"width":2000000,"height":800000}),None).unwrap();
        assert!(!tree.is_null());
        assert!(ui.view().0>before);
        assert_eq!(ui.height(),800.);
        assert_eq!(tree["child"]["style"]["height"],fill());
        assert_eq!(tree["child"]["children"][2]["style"]["height"],json!({"unit":"flex","value":1}));
        assert!(ui.event(&mut state,&mut host,&json!({"t":"size","id":"canvas","x":10000,"y":20000,"width":2000000,"height":800000}),None).unwrap().is_null());
        assert!(host.sent.is_empty());
    }

    #[test]
    fn the_same_layout_on_two_pages_keeps_each_page_s_own_canvas() {
        let mut ui=ready();let mut state=State::default();let mut host=Script::default();
        let size=|w:i64,h:i64| json!({"t":"size","id":"canvas","x":0,"y":0,"width":w*1000,"height":h*1000});
        ui.event(&mut state,&mut host,&size(600,360),Some("layout")).unwrap();
        ui.event(&mut state,&mut host,&size(1400,900),Some("layout-page")).unwrap();
        let wide=ui.menu_position(44.);
        // A tick for the settings page is answered with the settings canvas, not the wide one, and so
        // the menu does not jump between the corners of two pages with every tick.
        ui.event(&mut state,&mut host,&json!({"t":"tick","at":1}),Some("layout")).unwrap();
        assert_eq!(ui.bounds,[0.,0.,600.,360.]);
        assert_ne!(ui.menu_position(44.),wide);
        ui.event(&mut state,&mut host,&json!({"t":"tick","at":2}),Some("layout-page")).unwrap();
        assert_eq!(ui.bounds,[0.,0.,1400.,900.]);
        assert_eq!(ui.menu_position(44.),wide);
    }

    #[test]
    fn a_drag_on_one_page_is_not_disturbed_by_the_other_page_s_ticks_and_sizes() {
        let mut ui=ready();let mut state=State::default();let mut host=Script::default();
        let size=|w:i64,h:i64| json!({"t":"size","id":"canvas","x":0,"y":0,"width":w*1000,"height":h*1000});
        ui.event(&mut state,&mut host,&size(600,360),Some("layout")).unwrap();
        ui.event(&mut state,&mut host,&size(1400,900),Some("layout-page")).unwrap();
        ui.event(&mut state,&mut host,&json!({"t":"pointer","id":"desktop-1","phase":"down","x":100000,"y":0}),Some("layout")).unwrap();
        let during=ui.bounds;
        assert!(ui.event(&mut state,&mut host,&json!({"t":"tick","at":5000}),Some("layout-page")).unwrap().is_null());
        assert!(ui.event(&mut state,&mut host,&size(1400,900),Some("layout-page")).unwrap().is_null());
        assert_eq!(ui.bounds,during,"the other page must not swap the canvas under a gesture");
        let moved=ui.event(&mut state,&mut host,&json!({"t":"pointer","id":"desktop-1","phase":"move","x":160000,"y":0,"at":100}),Some("layout")).unwrap();
        assert!(!moved.is_null());
        assert_ne!(ui.items[1].at,Some([100,0]));
        ui.event(&mut state,&mut host,&json!({"t":"pointer","id":"desktop-1","phase":"cancel","x":160000,"y":0}),Some("layout")).unwrap();
        assert!(ui.grab.is_none());
        assert_eq!(ui.bounds,[0.,0.,600.,360.]);
        assert!(ui.event(&mut state,&mut host,&size(1400,900),Some("layout-page")).unwrap().is_null());
        assert_eq!(ui.bounds,[0.,0.,1400.,900.],"after the gesture the other page is served again");
    }

    #[test]
    fn dragging_from_the_menu_respects_pan_and_zoom() {
        let mut ui=ready();let mut state=State::default();let mut host=Script::default();
        ui.items[1].at=None;ui.expected=json!([]);ui.choosing=true;ui.zoom=Some((2.,[50.,30.]));
        pointer(&mut ui,&mut state,&mut host,"menu-device-1","down",40000,40000);
        pointer(&mut ui,&mut state,&mut host,"menu-device-1","move",210000,60000);
        assert_eq!(ui.items[1].at,Some([100,0]));
        assert!(host.sent.is_empty());
        pointer(&mut ui,&mut state,&mut host,"menu-device-1","cancel",210000,60000);
        assert_eq!(ui.items[1].at,None);
    }

    #[test]
    fn clicking_an_unplaced_device_row_places_it_without_an_add_button() {
        let mut ui=ready();let mut state=State::default();let mut host=Script::default();
        ui.items[1].at=None;ui.expected=json!([]);ui.choosing=true;
        host.results.push_back(json!({"status":"ok","value":scene()}));
        pointer(&mut ui,&mut state,&mut host,"menu-device-1","down",40000,40000);
        assert!(host.sent.is_empty());
        pointer(&mut ui,&mut state,&mut host,"menu-device-1","up",40000,40000);
        assert_eq!(ui.items[1].at,Some([100,0]));
        assert_eq!(host.sent.len(),1);
        assert_eq!(host.sent[0]["method"],"arrange");
        assert!(!ui.choosing);
    }

    #[test]
    fn dragging_an_unplaced_row_uses_canvas_coordinates_and_saves_only_on_drop() {
        let mut ui=ready();let mut state=State::default();let mut host=Script::default();
        ui.items[1].at=None;ui.expected=json!([]);ui.choosing=true;
        let menu_at=ui.menu_position(100.);
        pointer(&mut ui,&mut state,&mut host,"menu-device-1","down",40000,40000);
        pointer(&mut ui,&mut state,&mut host,"menu-device-1","move",160000,70000);
        assert_eq!(ui.items[1].at,Some([100,0]));
        assert_eq!(ui.menu_position(100.),menu_at,"the capture target must not move or disappear during a drag");
        assert!(host.sent.is_empty());
        host.results.push_back(json!({"status":"ok","value":scene()}));
        pointer(&mut ui,&mut state,&mut host,"menu-device-1","up",160000,70000);
        assert_eq!(host.sent.len(),1);
        assert_eq!(host.sent[0]["args"]["placements"],scene()["placements"]);
    }

    #[test]
    fn cancelling_or_dropping_outside_the_canvas_does_not_place_the_device() {
        for (phase,x,y) in [("cancel",160000,70000),("up",-100000,-100000)] {
            let mut ui=ready();let mut state=State::default();let mut host=Script::default();
            ui.items[1].at=None;ui.expected=json!([]);ui.choosing=true;
            pointer(&mut ui,&mut state,&mut host,"menu-device-1","down",40000,40000);
            pointer(&mut ui,&mut state,&mut host,"menu-device-1","move",160000,70000);
            pointer(&mut ui,&mut state,&mut host,"menu-device-1",phase,x,y);
            assert_eq!(ui.items[1].at,None);
            assert_eq!(ui.expected,json!([]));
            assert!(host.sent.is_empty());
        }
    }

    #[test]
    fn the_open_device_menu_keeps_its_corner_when_the_pointer_moves() {
        let mut ui=ready();let mut state=State::default();let mut host=Script::default();
        let closed=ui.menu_position(44.);
        ui.event(&mut state,&mut host,&json!({"t":"press","id":"devices"}),None).unwrap();
        let opened=ui.menu_position(100.);
        assert_eq!(opened[0],closed[0],"opening must not switch to the opposite corner");
        ui.pointer=Some([opened[0]+20.,opened[1]+20.]);
        assert_eq!(ui.menu_position(100.),opened);
    }

    #[test]
    fn drag_samples_keep_the_latest_position_without_serializing_more_than_one_frame_per_16ms() {
        let mut ui=ready();let mut state=State::default();let mut host=Script::default();
        pointer(&mut ui,&mut state,&mut host,"desktop-1","down",100000,0);
        let mut rendered=0;
        for n in 1..=240 {
            let tree=ui.event(&mut state,&mut host,&json!({"t":"pointer","id":"desktop-1","phase":"move","x":100000+n*1000,"y":0,"at":n*4}),None).unwrap();
            if !tree.is_null() {rendered+=1;}
        }
        assert!(rendered<=60, "high-rate input must not repaint above the frame cadence");
        assert_eq!(ui.items[1].at,Some([340,0]),"samples still update geometry between rendered frames");
        assert!(host.sent.is_empty(),"moving is not saving");
        assert!(!pointer(&mut ui,&mut state,&mut host,"desktop-1","cancel",340000,0).is_null());
        assert_eq!(ui.items[1].at,Some([100,0]),"cancel renders the restored geometry immediately");
    }

    #[test]
    fn the_corner_picker_avoids_devices_and_the_active_pointer_until_every_corner_is_occupied() {
        let mut ui=ready();
        assert_eq!(ui.menu_position(44.),[372.,8.],"left corners are occupied by devices");
        ui.pointer=Some([400.,20.]);
        assert_eq!(ui.menu_position(44.),[8.,308.],"the pointer excludes the other top corner");
        ui.items[0].displays[0].width=600;
        ui.items[0].displays[0].height=360;
        let at=ui.menu_position(44.);
        assert!(at[0]>=0. && at[0]+220.<=600. && at[1]>=0. && at[1]+44.<=ui.height());
    }

    #[test]
    fn negative_monitor_origins_normalize_without_changing_relative_positions() {
        let item=Item{device:"scoped".into(),name:"Remote".into(),local:false,at:Some([200,300]),
            displays:vec![Display{x:-100,y:-50,width:100,height:100},Display{x:0,y:0,width:200,height:150}]};
        let screens=item.screens(3);
        assert_eq!((screens[0].x,screens[0].y,screens[0].right(),screens[0].bottom()),(200,300,300,400));
        assert_eq!((screens[1].x,screens[1].y),(300,350));
        assert_eq!(item.extent(),[300,200]);
    }

    #[test]
    fn snapping_uses_screen_space_threshold_and_overlap_is_rejected() {
        let mut ui=ready();
        assert_eq!(ui.snap(1,[113,4],1.),[100,0]);
        assert_eq!(ui.snap(1,[113,4],2.),[113,0]);
        assert!(ui.verdict().0);
        ui.items[1].at=Some([50,0]);
        assert!(!ui.verdict().0);
        ui.items[1].at=Some([300,0]);
        assert!(ui.verdict().0);
    }

    #[test]
    fn mirrored_screens_of_one_device_do_not_block_placing_another() {
        let mut ui=Ui::default();
        ui.load(json!({"devices":[
            {"device":"this-device","name":"Local","displays":[
                {"x":-100,"y":-50,"width":100,"height":100},{"x":-100,"y":-50,"width":100,"height":100}],"at":[0,0],"local":true},
            {"device":"scoped","name":"Remote","displays":[{"x":0,"y":0,"width":100,"height":100}],"at":[100,0],"local":false}
        ],"placements":[{"device":"scoped","x":100,"y":0}]})).unwrap();
        assert!(ui.verdict().0, "a device's mirrored outputs share one place");
        ui.items[1].at=Some([50,0]);
        assert!(!ui.verdict().0, "two devices still may not overlap");
    }

    #[test]
    fn wheel_zoom_preserves_the_plane_point_under_the_pointer() {
        let mut ui=ready();let mut state=State::default();let mut host=Script::default();
        ui.event(&mut state,&mut host,&json!({"t":"wheel","id":"canvas","x":210000,"y":120000,"dy":1}),None).unwrap();
        let (scale,origin)=ui.view();
        assert!((origin[0]+200./scale-200.).abs()<1e-8);
        assert!((origin[1]+100./scale-100.).abs()<1e-8);
        assert!(host.sent.is_empty());
    }

    #[test]
    fn cancelled_or_overlapping_drag_restores_without_saving() {
        for phase in ["cancel","up"] {
            let mut ui=ready();let mut state=State::default();let mut host=Script::default();
            pointer(&mut ui,&mut state,&mut host,"desktop-1","down",100000,0);
            pointer(&mut ui,&mut state,&mut host,"desktop-1","move",50000,0);
            assert!(!ui.verdict().0);
            pointer(&mut ui,&mut state,&mut host,"desktop-1",phase,50000,0);
            assert_eq!(ui.items[1].at,Some([100,0]));
            assert!(ui.grab.is_none());
            assert!(host.sent.is_empty());
        }
    }

    #[test]
    fn rejected_save_rolls_back_and_keeps_the_optimistic_token() {
        let mut ui=ready();let mut state=State::default();let mut host=Script::default();
        host.results.push_back(json!({"status":"error","error":"refused"}));
        pointer(&mut ui,&mut state,&mut host,"desktop-1","down",100000,0);
        pointer(&mut ui,&mut state,&mut host,"desktop-1","move",300000,0);
        pointer(&mut ui,&mut state,&mut host,"desktop-1","up",300000,0);
        assert_eq!(ui.items[1].at,Some([100,0]));
        assert!(ui.error.is_some());
        assert_eq!(ui.expected,scene()["placements"]);
        assert_eq!(host.sent.len(),1);
    }

    #[test]
    fn a_zoomed_large_monitor_is_clipped_before_serializing_coordinates() {
        let mut ui=ready();
        ui.items[1].displays[0].width=50000;
        ui.zoom=Some((1.,[30000.,0.]));
        let tree=ui.tree();
        let cards=tree["child"]["children"][2]["children"].as_array().unwrap();
        let card=cards.iter().find(|node|node["id"]=="desktop-1").unwrap();
        assert_eq!(card["style"]["at"],json!([0,0]));
        assert_eq!(card["style"]["width"],px(600.));
    }
}
