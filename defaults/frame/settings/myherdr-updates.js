// Integration-owned distribution controls; stock Herdr keeps its original page.
import { div } from 'gpui-kit';
import { v_flex, h_flex } from 'gpui-base';
import { appHome, readTextFile, exec, updates } from 'herdr';

export class MyHerdrUpdates {
  constructor(){this.config=null;this.state={};this.enabled=true;this.busy=false;}
  refresh(cx){
    if(this.polling)return;this.polling=true;
    cx.spawn(async cx=>{try{
      this.config=JSON.parse(await readTextFile(appHome()+'/myherdr.json')??'null');
      if(this.config){
        this.state=JSON.parse(await readTextFile(this.config.data+'/updates/status.json')??'{}');
        this.enabled=JSON.parse(await readTextFile(this.config.data+'/updates/settings.json')??'{}').enabled!==false;
      }
    }catch{this.error='無法讀取更新狀態，請重試。';}finally{this.polling=false;cx.notify();}});
  }
  action(flag,cx){
    if(!this.config||this.busy)return;this.busy=true;this.error=null;cx.notify();
    cx.spawn(async cx=>{try{
      const result=await exec(this.config.launcher,[flag,'--data-dir',this.config.data],{timeout:590000});
      if(result.code!==0)throw Error('更新操作未完成，請重試。');
      this.refresh(cx);
    }catch{this.error='更新操作未完成，請重試。';}finally{this.busy=false;cx.notify();}});
  }
  description(){
    const s=this.state;
    if(s.state==='downloading')return `正在下載 ${s.available??''}：${s.total?Math.floor(100*s.downloaded/s.total):0}%`;
    if(s.state==='ready')return `${s.available??s.pending?.version} 已下載完成。`;
    if(s.state==='installing')return '正在安裝更新。';
    if(this.error)return this.error;
    if(s.error)return s.error.includes('GitHub')||s.error.includes('gh auth')?'請確認 GitHub CLI 登入及網路連線後重試。':'更新未完成，可按「檢查更新」重試。';
    if(s.state==='checking'||this.busy)return '正在檢查更新…';
    return s.checked?`已是最新版本。上次檢查：${new Date(s.checked).toLocaleString()}`:'尚未檢查更新。';
  }
  render(t,ui){
    const u=updates();
    const buttons=h_flex().gap(8).flex_none()
      .child(ui.button(t,'st-myherdr-check',this.busy?'處理中…':'檢查更新',(_e,cx)=>this.action('--update-check',cx)))
      .when(this.state.state==='ready',el=>el.child(ui.button(t,'st-myherdr-apply','重新啟動並更新',(_e,cx)=>this.action('--update-apply',cx))));
    return v_flex().w_full()
      .child(ui.heading(t,'My Herdr'))
      .child(ui.row(t,`My Herdr ${this.config.version}`,this.description(),buttons))
      .child(ui.row(t,'自動更新','啟動後及每小時檢查，在背景下載新版。',ui.toggle(t,'st-myherdr-auto',this.enabled,(_e,cx)=>this.action(this.enabled?'--update-disable':'--update-enable',cx))))
      .child(ui.row(t,'配對版本',`Herdr ${u.app.version} · herdr-core ${u.core.version??''}`,div()))
      .child(div().pt(12).text_size(12.5).line_height('18px').text_color(t.dim).child('App 與 Core 會一起更新。重新啟動更新前會確認是否結束此 session 的 terminal／agent，草稿、歷史與設定會保留。私人更新使用已登入的 GitHub CLI。'));
  }
}
